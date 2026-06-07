mod calendar;
mod html;
mod interpolation;
mod key_value_pair;

pub use calendar::{Calendar, CalendarEvent};
pub use html::html_to_markdown;
pub use interpolation::interpolate;
pub use key_value_pair::parse_key_value_pairs;
