//! Length-prefixed JSON frames: a `u32` in native byte order, then that many
//! bytes of UTF-8 JSON. This is the browser native messaging format.

use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::{Error, Result};

/// Largest frame accepted or sent (8 MiB). Browsers cap host-to-extension
/// messages at 1 MiB; requests are far smaller than either limit.
pub const MAX_FRAME: usize = 8 << 20;

/// Reads one frame. `Ok(None)` on a clean end of stream before a frame.
pub async fn read_frame<R, T>(reader: &mut R) -> Result<Option<T>>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let mut len = [0u8; 4];
    match reader.read_exact(&mut len).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let len = u32::from_ne_bytes(len) as usize;
    if len > MAX_FRAME {
        return Err(Error::TooLarge(len));
    }
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf).await?;
    Ok(Some(serde_json::from_slice(&buf)?))
}

/// Writes one frame and flushes.
pub async fn write_frame<W, T>(writer: &mut W, value: &T) -> Result<()>
where
    W: AsyncWrite + Unpin,
    T: Serialize + ?Sized,
{
    let json = serde_json::to_vec(value)?;
    if json.len() > MAX_FRAME {
        return Err(Error::TooLarge(json.len()));
    }
    let len = u32::try_from(json.len()).map_err(|_| Error::TooLarge(json.len()))?;
    writer.write_all(&len.to_ne_bytes()).await?;
    writer.write_all(&json).await?;
    writer.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Request;

    #[tokio::test]
    async fn roundtrip_and_eof() {
        let mut buf = Vec::new();
        write_frame(&mut buf, &Request::Ping).await.unwrap();
        write_frame(&mut buf, &Request::Ping).await.unwrap();
        assert_eq!(&buf[..4], &15u32.to_ne_bytes());
        let mut r = buf.as_slice();
        assert_eq!(
            read_frame::<_, Request>(&mut r).await.unwrap(),
            Some(Request::Ping)
        );
        assert_eq!(
            read_frame::<_, Request>(&mut r).await.unwrap(),
            Some(Request::Ping)
        );
        assert_eq!(read_frame::<_, Request>(&mut r).await.unwrap(), None);
    }

    #[tokio::test]
    async fn rejects_oversized_and_truncated_frames() {
        let mut big = (u32::MAX).to_ne_bytes().to_vec();
        big.extend_from_slice(b"{}");
        assert!(matches!(
            read_frame::<_, Request>(&mut big.as_slice()).await,
            Err(Error::TooLarge(_))
        ));
        let mut short = 100u32.to_ne_bytes().to_vec();
        short.extend_from_slice(b"{\"type\"");
        assert!(matches!(
            read_frame::<_, Request>(&mut short.as_slice()).await,
            Err(Error::Io(_))
        ));
        let mut junk = 3u32.to_ne_bytes().to_vec();
        junk.extend_from_slice(b"abc");
        assert!(matches!(
            read_frame::<_, Request>(&mut junk.as_slice()).await,
            Err(Error::Json(_))
        ));
    }
}
