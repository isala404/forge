# todoapp React frontend

A single Vite React app for the Rust, Node, and Python REST backends.

```sh
bun install
bun run dev --host 127.0.0.1 --port 5174
```

Open with one of:

- `http://127.0.0.1:5174/?api=http://127.0.0.1:9081`
- `http://127.0.0.1:5174/?api=http://127.0.0.1:9082`
- `http://127.0.0.1:5174/?api=http://127.0.0.1:9083`

Run e2e after the three backends and frontend are running:

```sh
bun run test:e2e
```
