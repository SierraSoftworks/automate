mod calendar;
mod html;
mod interpolation;

pub use calendar::{Calendar, CalendarEvent};
pub use html::html_to_markdown;
pub use interpolation::interpolate;
