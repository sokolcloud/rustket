//! Message search across the rooms of one server: the query language
//! ([`query`]), local matching and highlighting ([`matcher`]) and the paced
//! fan-out with merged results ([`run`]). Nothing here is stored.

pub mod matcher;
pub mod query;
pub mod run;

#[cfg(test)]
mod tests;

pub use matcher::{LocalMatcher, MatchCtx, highlight, snippet};
pub use query::{Has, SearchQuery, format_date, parse_date};
pub use run::{
    Backend, GLOBAL_LIMIT, Hit, Next, Origin, PER_ROOM_LIMIT, SearchRun, Target, resolve_rooms,
    rooms_to_search,
};
