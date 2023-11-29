mod settings;

use settings::Settings;

fn main() {
    let settings = Settings::new();
    let settings = match settings {
        Ok(s) => s,
        Err(_) => {
            println!("Could not read settings. Please provide the settings file 'clrigctl.toml'.");
            return;
        }
    };

    println!("{:?}", settings);
}
