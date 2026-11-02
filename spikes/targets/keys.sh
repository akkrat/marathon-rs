#!/usr/bin/env bash
# Generates the throwaway client keys of matrix row T13 into $SPIKE_TMP/keys. OpenSSH formats come from the local
# ssh-keygen; the PuTTY files come from puttygen in a disposable Alpine container, so nothing is installed on the host.
set -euo pipefail
SPIKE_TMP=${SPIKE_TMP:-/tmp/ssh-spike}
dir=$SPIKE_TMP/keys
rm -rf "$dir"
mkdir -p "$dir"
cd "$dir"

passphrase=spike-passphrase
ssh-keygen -q -t ed25519 -N '' -C spike-ed25519 -f ed25519_openssh
ssh-keygen -q -t ecdsa -b 256 -N '' -C spike-ecdsa -f ecdsa_openssh
ssh-keygen -q -t rsa -b 3072 -N '' -C spike-rsa -f rsa_openssh
cp rsa_openssh rsa_pem && cp rsa_openssh.pub rsa_pem.pub
ssh-keygen -q -p -m PEM -N '' -f rsa_pem
cp rsa_openssh rsa_pkcs8 && cp rsa_openssh.pub rsa_pkcs8.pub
ssh-keygen -q -p -m PKCS8 -N '' -f rsa_pkcs8
ssh-keygen -q -t ed25519 -N "$passphrase" -C spike-ed25519-encrypted -f ed25519_openssh_encrypted
cp rsa_openssh rsa_pem_encrypted && cp rsa_openssh.pub rsa_pem_encrypted.pub
ssh-keygen -q -p -m PEM -N "$passphrase" -f rsa_pem_encrypted

printf '%s\n' "$passphrase" > passphrase.txt
# The colima VM does not share this directory, so the files travel through tar on stdin and stdout.
tar -cf - ed25519_openssh rsa_openssh passphrase.txt | docker run -i --rm alpine:3.20 sh -euc '
    apk add --no-cache -q putty >/dev/null
    mkdir /k && cd /k && tar -xf -
    puttygen ed25519_openssh -O private -o ed25519_v3.ppk
    puttygen ed25519_openssh -O private --ppk-param version=2 -o ed25519_v2.ppk
    puttygen rsa_openssh -O private -o rsa_v3.ppk
    puttygen rsa_openssh -O private --ppk-param version=2 -o rsa_v2.ppk
    puttygen ed25519_openssh -O private --new-passphrase passphrase.txt -o ed25519_v3_encrypted.ppk
    chmod 600 ./*.ppk
    tar -cf - ./*.ppk
' | tar -xf -
for ppk in ed25519_v3 ed25519_v2 ed25519_v3_encrypted; do cp ed25519_openssh.pub "$ppk.ppk.pub"; done
for ppk in rsa_v3 rsa_v2; do cp rsa_openssh.pub "$ppk.ppk.pub"; done
cat ./*.pub | sort -u > authorized_keys
# Host key of the fake worker (ssh-russh-spike serve); kept out of authorized_keys
ssh-keygen -q -t ed25519 -N '' -C spike-fake-host -f fake_host_ed25519
ls -1 "$dir"
