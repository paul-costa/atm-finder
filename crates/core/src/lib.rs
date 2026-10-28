//! atm-finder core: everything that does not touch the network or the file system.
//!
//! The crate is shared by the command-line tool (blocking HTTP) and the website (WebAssembly,
//! browser fetch), so it only builds queries and parses responses; whoever uses it sends the
//! requests. The modules follow the steps of an export:
//!
//! 1. [`boundary`] finds the administrative area the user asked for and its polygon.
//! 2. [`model`] holds the ATM record, parses the OpenStreetMap response and merges duplicates;
//!    [`sources`] and [`firecrawl`] parse the other sources.
//! 3. [`addresses`] completes missing addresses from nearby house numbers.
//! 4. [`export`] writes CSV, GeoJSON and KML.
//!
//! [`geo`] has the geometry underneath and [`places`] the place lists of the batch export.

pub mod addresses;
pub mod boundary;
pub mod export;
pub mod firecrawl;
pub mod geo;
pub mod model;
pub mod places;
pub mod sources;

/// Default merge radius in metres: records of the same operator closer than this count as one ATM.
pub const MERGE_RADIUS_M: f64 = 40.0;
/// Search radius in metres for the nearest house number when completing addresses.
pub const ADDRESS_RADIUS_M: f64 = 50.0;
