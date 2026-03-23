# Fawx Weather Skill

WASM skill plugin for [Fawx](https://github.com/fawxai/fawx).

## Build

```bash
cargo build --release --target wasm32-unknown-unknown
```

## Install

```bash
fawx skill install ./target/wasm32-unknown-unknown/release/weather_skill.wasm
```

## License

Apache 2.0
