//! `rdm-host` library: identifiers shared with the extension and the
//! per-browser native messaging manifest installer. The `rdm-host` binary
//! (the host itself) and the app's Settings both use it.

pub mod install;

/// Native messaging host name, as referenced by the extension.
pub const HOST_NAME: &str = "dev.rdm.host";

/// Chrome-family extension IDs allowed to talk to the host. The first is
/// the development ID fixed by the `key` in `extension/manifest.json`; a
/// store listing gets its own ID, which is added here.
pub const CHROME_EXTENSION_IDS: &[&str] = &["dacdblamhhaedfahmbcllbpgjejdaddi"];

/// Firefox add-on ID (`browser_specific_settings.gecko.id`).
pub const FIREFOX_EXTENSION_ID: &str = "rdm@rdm.dev";

/// Overrides the app executable the host launches (tests, development).
pub const APP_ENV: &str = "RDM_APP";
