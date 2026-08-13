#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod config;
mod discovery;
mod network;
mod platform;
mod protocol;

fn main() {
    app::run();
}
