setup:
    rustup target add wasm32-unknown-unknown
    cargo install wasm-pack

wasm:
	cd ui && wasm-pack build --target web --release
	cd ui && cargo build --release --bins

clean:
    cd ui && cargo clean
    rm -rf ui/pkg

