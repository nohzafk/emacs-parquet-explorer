setup:
    rustup target add wasm32-unknown-unknown
    cargo install wasm-pack

wasm:
    cd renderer && wasm-pack build --target web --release
