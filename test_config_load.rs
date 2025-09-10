use nostr_push_service::config::Settings;

fn main() {
    match Settings::new() {
        Ok(settings) => {
            println!("✓ Config loaded successfully");
            println!("  control_kinds: {:?}", settings.service.control_kinds);
            println!("  dm_kinds: {:?}", settings.service.dm_kinds);
        }
        Err(e) => {
            eprintln!("✗ Failed to load config: {}", e);
            std::process::exit(1);
        }
    }
}
