//! Heap-allocated collections.
//!
//! Hash collections use hashbrown's default hasher without requiring an OS
//! randomness source. It does not provide the standard library's HashDoS
//! resistance; supply a suitable hasher when processing untrusted keys.

pub use alloc::collections::*;
pub use hashbrown::{hash_map, hash_set, HashMap, HashSet};
