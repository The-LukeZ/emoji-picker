//! Platform-free core: emoji table, asset bundle format, hotkey, config and recents.

pub mod config;
pub mod geometry;
pub mod hotkey;
pub mod picker_state;
pub mod policy;
pub mod recents;
pub mod search;
pub mod table;

#[cfg(test)]
mod test_fixture;
