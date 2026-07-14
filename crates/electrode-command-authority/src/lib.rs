//! Electrode command authority.
//!
//! Browser peers and native vehicle peers use separate Zenoh sessions. The
//! browser-to-vehicle paths are the typed mappings in [`CommandPolicy`], plus
//! the schema-verified private simulator MocapFrame relay.

mod policy;
mod runtime;
mod velocity_budget;

pub use policy::{AuthorizedCommand, CommandPolicy, Delivery, PolicyConfig, PolicyError};
pub use runtime::{CommandAuthority, CommandAuthorityConfig};
