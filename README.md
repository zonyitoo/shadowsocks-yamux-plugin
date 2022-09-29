# shadowsocks-yamux-plugin

A shadowsocks SIP003 Plugin with connection multiplexor in YAMUX protocol

```
ClientA ----+                                                                                    +---- RemoteA
            |  N Connections                   M Connections                      N Connections  |
ClientB ----+---- sslocal ---- yamux-plugin-local -------- yamux-plugin-server ---- ssserver ----+---- RemoteB
            |                                                                                    |
ClientC ----+                                                                                    +---- RemoteC
```

`yamux-plugin` could mulplex `N` TCP connections into `M` TCP tunnels, which `N >= M`.

## Build

```bash
cargo build --release
```
