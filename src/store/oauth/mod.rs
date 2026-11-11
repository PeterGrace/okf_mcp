//! The OAuth tables (M10): clients (`clients`), sign-in codes (`codes`) and
//! tokens (`tokens`). Grants are `api_keys` rows of kind `oauth`
//! (`store::keys`).

pub mod clients;
pub mod codes;
#[cfg(test)]
pub(crate) mod testing;
pub mod tokens;
