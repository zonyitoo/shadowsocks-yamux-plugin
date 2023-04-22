# shadowsocks-yamux-plugin

A shadowsocks SIP003 (SIP003u) Plugin with connection multiplexor in YAMUX protocol

```plain
ClientA ----+
            |  N Connections
ClientB ----+---- sslocal ---- yamux-plugin-local
            |                           |
ClientC ----+                           |
                                        | M (TCP) Connections
RemoteA ----+                           |
            |                           |
RemoteB ----+---- ssserver ---- yamux-plugin-server
            |  N Connections
RemoteC ----+
```

`yamux-plugin` could mulplex `N` TCP / UDP connections into `M` TCP tunnels, which `N >= M`.

## Build

```bash
cargo build --release
```

## Plugin Options

```json
{
    "plugin_opts": "outbound_fwmark=100&outbound_user_cookie=100&outbound_bind_interface=eth1&outbound_bind_addr=1.2.3.4"
}
```

* `outbound_fwmark`: Linux (or Android) sockopt `SO_MARK`
* `outbound_user_cookie`: FreeBSD sockopt `SO_USER_COOKIE`
* `outbound_bind_interface`: Socket binds to interface, Linux `SO_BINDTODEVICE`, macOS `IP_BOUND_IF`, Windows `IP_UNICAST_IF`
* `outbound_bind_addr`: Socket binds to IP
* `udp_timeout`: UDP tunnel timeout (default 5 minutes)
* `tcp_keep_alive`: TCP socket keep-alive time (default 15 seconds)
* `tcp_fast_open`: TCP Fast Open
* `mptcp`: Multipath-TCP
* `ipv6_first`: Connect IPv6 first (default true)

## SAFETY WARNING

This plugin will add a fixed magic number as a header of all YAMUX connections, which could be detected easily.
