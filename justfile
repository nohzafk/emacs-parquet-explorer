setup:
    rustup target add wasm32-unknown-unknown
    cargo install wasm-pack

wasm:
    cd ui && wasm-pack build --target web --release
