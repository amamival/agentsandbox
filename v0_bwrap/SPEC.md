# SPEC

## 概要

`sandbox.sh` は、このリポジトリの NixOS 設定を使って、ローカルマシン上に開発用の隔離環境を構築し、起動と再接続を行うランチャーである。

このランチャーは次を一体で扱う。

- Nix ベース sysroot の取得
- NixOS システムのビルド
- `pasta` と `bubblewrap` を使った namespace 隔離起動
- 起動済み systemd への再接続

実装上の特徴として、container 内 root には `--map-auto --setuid=0 --setgid=0` を使った isolated root を採用する。これにより container 内 root は host 側では subuid/subgid range に対応し、host 実ユーザーそのものにはならない。`--map-root-user` を使わない点は、この隔離モデルを支える feature の一つである。

また、`bubblewrap` は bind mount 元パスを host 側で canonicalize するため、`$HOME` 配下の `0700` パスに state を置くと isolated root から到達できず起動前に失敗する。このため state は host namespace 側で `/sandbox` に集約する。

## 必要コマンド

- `bash`
- `curl`
- `tar`
- `jq`
- `unshare`
- `pasta`
- `bwrap`
- `sudo`
- `pgrep`
- `nsenter`
- `awk`
- `chmod`
- `chown`

## ディレクトリ構成

ベースディレクトリは `/sandbox` とする。

- `/sandbox/`
  - mode: `0755`
  - host 側の固定作業ディレクトリ
- `/sandbox/sysroot/`
  - Docker Hub から展開した Nix ベースイメージとビルド済みシステムの格納先
- `/sandbox/persistent/`
  - container 内 `/persistent` に bind する永続領域
- `/sandbox/sysroot.pid`
  - 起動中の `unshare` 側プロセス PID を書く

## 初回セットアップ

`/sandbox/sysroot.pid` が存在しない場合だけ `prepare sysroot persistent sysroot.pid` を行う。

1. host namespace 側で `sudo install -d -m 777 /sandbox` を実行する
2. `nssudo` で `sysroot` と `persistent` を作成する
3. `nssudo` で `sysroot.pid` を作成する
4. host namespace 側で `/sandbox` の mode を `0755` に戻す

この順序により、初回だけ到達性を開けつつ、セットアップ後は固定パスとして閉じ直す。

## 起動仕様

### `nssudo`

- `unshare --map-auto --setuid=0 --setgid=0 --wd /sandbox "$@"` を実行する
- 必要に応じて `nssudo --map-current-user ...` を使い、host 実ユーザーを namespace 内 root に対応付けた操作を行う
- namespace 内 root は host 側では subuid/subgid range に写る

### `fetch_nixos_dockerhub SYSROOT`

- `nixos/nix:latest` の `linux/amd64` manifest を Docker Hub API から取得する
- 各 layer blob を download し、`nssudo tar zxf - -C "$SYSROOT"` で展開する

### `install_nixos SYSROOT`

- `SYSROOT/etc/nixos/` を `nssudo` で作成する
- `flake.nix` と `configuration.nix` を一度 `/tmp` にコピーし、そこから `SYSROOT/etc/nixos/` に install する
- `SYSROOT/nix/var/nix/profiles/system` を削除して張り直す
- `nssudo bwrap` で `SYSROOT` を `/` に bind し、`nix build` を実行する
- `Python` の `_multiprocessing.SemLock` 向けに `--perms 1777 --tmpfs /dev/shm` を付ける

### `start_container SYSROOT PERSISTENT PIDFILE`

- 起動前に `chmod o+rw "$(tty)"` を実行し、id-mapped container systemd が `bwrap` の `/dev/console` bind 経由で host tty を開けるようにする
- 現在の host cgroup path を `/proc/self/cgroup` から求め、`CURRENT_CGROUP=/sys/fs/cgroup...` として解決する
- `nssudo --map-current-user chown -R 0:0 "$CURRENT_CGROUP"` を実行し、その cgroup subtree を namespace 内 root 所有に寄せる
- `nssudo /bin/sh -c 'echo $$ > "$PIDFILE"; exec "$@"'` で PID を記録してから本体を exec する
- `pasta --foreground --config-net --map-host-loopback 10.0.2.2 --tcp-ports 2222:22 --netns-only` を使う
- `bwrap` では次を行う
  - `--die-with-parent`
  - `--unshare-pid --unshare-ipc --unshare-uts --unshare-cgroup`
  - `--tmpfs /`
  - `--dev /dev --proc /proc`
  - `--ro-bind /sys /sys`
  - `--bind "$CURRENT_CGROUP" /sys/fs/cgroup`
  - `--bind "$SYSROOT/nix" /nix`
  - `--bind "$PERSISTENT" /persistent`
  - `--ro-bind /etc/resolv.conf /etc/resolv.conf`
  - `--clearenv --new-session --as-pid-1`
- PID 1 は `/nix/var/nix/profiles/system/init` を実行する

### `attach PID`

- `sysroot.pid` に記録された PID を起点に、同じ user namespace の systemd を探す
- `nsenter -t "$PID" -U -m -n -p -i -u` で user, mount, network, pid, ipc, uts namespace に入る

## セキュリティ前提

- `--map-root-user` は使わない
- `--map-auto --setuid=0 --setgid=0` では namespace 内 root は host の subuid/subgid range に対応する
- そのため、仮に隔離を抜けても host 側では実ユーザー権限ではなく、秘密情報の直接読取りを避ける前提を置く
- host 側で必要な privileged 操作は `/sandbox` の準備、起動 tty の mode 緩和、現在の cgroup subtree の所有権調整に限定する

## 既知の前提

- systemd 起動には host tty と cgroup subtree の整合が必要である
- `start_container` は host tty に `o+rw` を付け、current cgroup subtree の owner を調整してから起動する
- tty mode と cgroup owner を元に戻す cleanup は現時点では実装していない
- `tty` が取れない非対話環境では起動できない可能性がある

## 制約

- `fetch_nixos_dockerhub` は `nixos/nix:latest` の `linux/amd64` を固定で取得する
- Docker Hub API の rate limit やレスポンス形式変更の影響を受ける
- `sandbox.sh` はファイアウォールや永続化ポリシー全体までは扱わない
- `sysroot.pid` が stale の場合は `kill -0` で生存確認してから再起動する
