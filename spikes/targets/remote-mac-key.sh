#!/usr/bin/env bash
# remote-mac-key.sh <command> [args]: runs the command with a throwaway key authorised on the remote Mac, then removes it.
#   1. a fresh ed25519 key pair in a temporary directory outside the repository;
#   2. its public key appended to ~/.ssh/authorized_keys on the remote Mac, tagged with a unique comment;
#   3. the command runs with SPIKE_AUTH=key:<throwaway key> and SPIKE_REMOTE_MAC_KEY_TAG set;
#   4. on exit, also on failure or interrupt, exactly the tagged line is removed (the file keeps its inode and mode),
#      the removal is verified, and the key files are deleted.
# Approved by the user for WP 0.3a; nothing else on the remote Mac changes.
set -euo pipefail

tag="marathon-rs-spike-$(openssl rand -hex 6)"
dir=$(mktemp -d /tmp/marathon-rs-spike-key.XXXXXX)
ssh-keygen -q -t ed25519 -N '' -C "$tag" -f "$dir/key"

existed=$(ssh remote-mac 'test -f ~/.ssh/authorized_keys && echo yes || echo no')
before=$(ssh remote-mac 'if test -f ~/.ssh/authorized_keys; then cksum < ~/.ssh/authorized_keys; fi')

cleanup() {
    local status=$?
    # grep -v exits 1 when nothing is left; cat > keeps the file's inode and permissions
    ssh remote-mac "t=\$(mktemp) && { grep -v -F '$tag' ~/.ssh/authorized_keys > \"\$t\" || true; } && cat \"\$t\" > ~/.ssh/authorized_keys && rm -f \"\$t\"" || true
    if [[ $existed == no ]]; then
        ssh remote-mac "test -s ~/.ssh/authorized_keys || rm -f ~/.ssh/authorized_keys" || true
    fi
    after=$(ssh remote-mac 'if test -f ~/.ssh/authorized_keys; then cksum < ~/.ssh/authorized_keys; fi')
    # Decided on output: `ssh remote-mac` reaches the Mac's Tailscale SSH server, which reports exit status 0 for every command
    remaining=$(ssh remote-mac "if test -f ~/.ssh/authorized_keys; then grep -c -F '$tag' ~/.ssh/authorized_keys; else echo 0; fi")
    if [[ $remaining != 0 || ($existed == yes && $after != "$before") ]]; then
        echo "ERROR: the throwaway key $tag is still in authorized_keys on the remote Mac" >&2
        status=1
    else
        echo "remote-mac-key: removed $tag; authorized_keys is back to its previous checksum" >&2
    fi
    rm -rf "$dir"
    exit $status
}
trap cleanup EXIT INT TERM

if [[ $existed == yes ]]; then
    ssh remote-mac "cat >> ~/.ssh/authorized_keys" < "$dir/key.pub"
else
    ssh remote-mac "test -d ~/.ssh || mkdir -m 700 ~/.ssh; umask 077 && cat > ~/.ssh/authorized_keys" < "$dir/key.pub"
fi
echo "remote-mac-key: added $tag (authorized_keys existed before: $existed)" >&2

SPIKE_AUTH="key:$dir/key" SPIKE_REMOTE_MAC_KEY_TAG=$tag "$@"
