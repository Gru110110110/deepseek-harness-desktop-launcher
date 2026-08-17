fn main() {
    match dsh_core::runtime::verify_release_sources() {
        Ok(sources) => {
            for source in sources {
                println!("verified: {source}");
            }
        }
        Err(error) => {
            eprintln!("release source verification failed: {error}");
            std::process::exit(1);
        }
    }
}
