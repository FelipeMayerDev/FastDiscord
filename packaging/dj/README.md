# DJ relay with Docker Compose

Copy these files and the compiled Linux x86_64 `fastdiscord-dj-server` binary
to the service directory. The binary must be built for glibc <= 2.36, as the
runtime image is Debian Bookworm. The current release binary requires glibc 2.34.
Create `.env` from `.env.example`, set a random relay key, and restrict it to
mode 600. Never use a Discord token as the relay key.

```sh
docker compose up -d --build
docker compose ps
docker compose logs --tail=30
```

The relay binds inside the container on port 8787, published only on host
loopback. Route a Cloudflare Tunnel hostname to `http://localhost:8787` when
cloudflared uses host networking. Clients use `wss://<hostname>` and the shared
key. No data volume is needed: sessions are ephemeral, and a restart clears
active sessions and playlists. The relay runs without root or write access.

For updates, replace the binary and run `docker compose up -d --build` again.
This may briefly interrupt playback. Keep the previous binary for rollback.
Detailed client behavior and trust model: [docs/DJ.md](../../docs/DJ.md).
