//! The OAuth tables (M10): clients (`clients`), sign-in codes (`codes`),
//! self-issued grants from consent as a signed-in user (`grants`) and
//! tokens (`tokens`). Grants are `api_keys` rows of kind `oauth`
//! (`store::keys`).

pub mod clients;
pub mod codes;
pub mod grants;
#[cfg(test)]
pub(crate) mod testing;
pub mod tokens;
