FROM debian:bookworm-slim
COPY --chmod=0555 fastdiscord-dj-server /usr/local/bin/fastdiscord-dj-server
USER 65532:65532
EXPOSE 8787
ENTRYPOINT ["/usr/local/bin/fastdiscord-dj-server"]
