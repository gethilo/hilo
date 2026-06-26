fn main() {
    match uniffi_build::generate_scaffolding("src/hilo.udl") {
        Ok(_) => {}
        Err(e) => {
            eprintln!("UniFFI error: {:?}", e);
            eprintln!("UniFFI error display: {}", e);
            std::process::exit(1);
        }
    }
}
