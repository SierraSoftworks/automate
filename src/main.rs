mod collectors;
mod db;
mod services;
mod parsers;
mod publishers;

use crate::db::*;

#[cfg(test)]
mod testing;

fn main() {
    let db = db::open("database.sqlite").unwrap();

    println!("Hello, world!");
}
