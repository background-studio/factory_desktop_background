#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[tokio::main]
async fn main() {
    if let Err(error) = factory_background_studio_lib::run().await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
