//! Platform-free core: emoji table, asset bundle format, hotkey, config and recents.

pub mod config;
pub mod geometry;
pub mod hotkey;
pub mod policy;
pub mod recents;
pub mod table;

#[cfg(test)]
mod test_fixture;
