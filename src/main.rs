mod settings;

use settings::Settings;

fn main() {
    let settings = Settings::new()
        .expect("Could not read settings.");

    println!("{:?}", settings);
}
