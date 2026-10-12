mod app;
mod completer;
mod config;
mod crypto;
mod db;
mod editor;
mod exporter;
mod history;
mod schema;
mod ui;

use app::App;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut app = App::new()?;
    app.run()?;
    Ok(())
}
