//! AES-128-CBC segment decryption (HLS `METHOD=AES-128`).
//!
//! This is the standard, non-DRM HLS encryption whose key is served to any
//! client by URL. DRM schemes (SAMPLE-AES, FairPlay, Widevine, PlayReady)
//! are rejected before anything is downloaded.

use aes::cipher::block_padding::Pkcs7;
use aes::cipher::{BlockModeDecrypt, KeyIvInit};

use crate::error::{Error, Result};

type Decryptor = cbc::Decryptor<aes::Aes128>;

/// Decrypts a whole segment and strips its PKCS#7 padding.
pub(crate) fn decrypt_aes128(mut data: Vec<u8>, key: &[u8; 16], iv: &[u8; 16]) -> Result<Vec<u8>> {
    if !data.len().is_multiple_of(16) {
        return Err(Error::Decrypt(format!(
            "{} bytes is not a whole number of AES blocks",
            data.len()
        )));
    }
    let len = Decryptor::new(&(*key).into(), &(*iv).into())
        .decrypt_padded::<Pkcs7>(&mut data)
        .map_err(|_| Error::Decrypt("bad padding (wrong key or IV?)".into()))?
        .len();
    data.truncate(len);
    Ok(data)
}

/// A 16-byte key from a key URI response.
pub(crate) fn key_from(body: &[u8]) -> Result<[u8; 16]> {
    body.try_into()
        .map_err(|_| Error::Decrypt(format!("key is {} bytes, expected 16", body.len())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes::cipher::BlockModeEncrypt;

    #[test]
    fn roundtrip_and_errors() {
        let key = [7u8; 16];
        let iv = 42u128.to_be_bytes();
        let plain = b"segment payload that is not block aligned".to_vec();
        let enc = cbc::Encryptor::<aes::Aes128>::new(&key.into(), &iv.into())
            .encrypt_padded_vec::<Pkcs7>(&plain);
        assert_eq!(decrypt_aes128(enc.clone(), &key, &iv).unwrap(), plain);
        assert_ne!(decrypt_aes128(enc, &[8u8; 16], &iv).ok(), Some(plain));
        assert!(decrypt_aes128(vec![0; 15], &key, &iv).is_err());
        assert!(key_from(&[0; 15]).is_err());
        assert_eq!(key_from(&[1; 16]).unwrap(), [1; 16]);
    }
}
