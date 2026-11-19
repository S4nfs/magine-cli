#!/bin/sh
# Magine CLI installer for macOS and Linux:
#
#   curl -fsSL https://magine.cloud/install.sh | sh
#
# Downloads the native binary (the npm package @magine/cli-<os>-<arch>), checks its
# sha512 against the npm registry, and puts it in ~/.magine/bin. No Node.js needed,
# nothing runs as root, and it never signs you in by itself.
#
#   MAGINE_VERSION=1.7.5       install this version (default: latest)
#   MAGINE_INSTALL_DIR=<dir>   where the binary goes (default: ~/.magine/bin)
#   MAGINE_NO_MODIFY_PATH=1    do not add that folder to your shell's PATH
set -eu

if [ -t 1 ] && [ -z "${NO_COLOR:-}" ] && [ "${TERM:-}" != dumb ]; then
  case "${COLORTERM:-}" in
    truecolor | 24bit) cat_c='\033[38;2;163;113;247m' ok_c='\033[38;2;63;185;80m' link_c='\033[38;2;121;192;255m' ;;
    *) cat_c='\033[35m' ok_c='\033[32m' link_c='\033[36m' ;;
  esac
  bold='\033[1m' dim='\033[2m' off='\033[0m'
else
  cat_c='' ok_c='' link_c='' bold='' dim='' off=''
fi

say() { printf '%s\n' "$*"; }
# paint COLOR TEXT: TEXT is printed as is (printf %s), only COLOR is interpreted.
paint() { printf "$1%s$off" "$2"; }
fail() { printf 'magine install: %s\n' "$*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || fail "this installer needs '$1'. Install it and run again, or use: npm install -g magine"; }

# The first "NAME":"value" in a JSON document (npm's version manifests are flat enough).
json_field() {
  printf '%s' "$2" | tr ',{}' '\n\n\n' | sed -n "s/^ *\"$1\" *: *\"\([^\"]*\)\".*/\1/p" | head -n 1
}

sha512_hex() {
  if command -v sha512sum >/dev/null 2>&1; then
    sha512sum "$1" | cut -d ' ' -f 1
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 512 "$1" | cut -d ' ' -f 1
  elif command -v openssl >/dev/null 2>&1; then
    openssl dgst -sha512 -binary "$1" | od -An -v -tx1 | tr -d ' \n'
  else
    fail "this installer needs sha512sum, shasum or openssl to check the download"
  fi
}

# npm publishes "sha512-<base64>"; both sides are compared as hex.
base64_to_hex() {
  for decode in "base64 -d" "base64 -D" "openssl base64 -d -A"; do
    hex="$(printf '%s' "$1" | $decode 2>/dev/null | od -An -v -tx1 | tr -d ' \n')"
    if [ "${#hex}" -eq 128 ]; then
      printf '%s' "$hex"
      return 0
    fi
  done
  fail "could not read the checksum the registry published"
}

fetch() {
  curl --fail --silent --show-error --location --proto "$proto" --tlsv1.2 --retry 2 "$@"
}

main() {
  registry="${MAGINE_NPM_REGISTRY:-https://registry.npmjs.org}"
  registry="${registry%/}"
  version="${MAGINE_VERSION:-latest}"
  dir="${MAGINE_INSTALL_DIR:-$HOME/.magine/bin}"

  proto='=https'
  case "$registry" in
    https://*) ;;
    http://localhost:* | http://127.0.0.1:*) proto='=http,https' ;;
    *) fail "MAGINE_NPM_REGISTRY must start with https://" ;;
  esac
  case "$dir" in
    *[\"\`\$\\]*) fail "MAGINE_INSTALL_DIR must not contain quotes, backticks, \$ or backslashes" ;;
    /*) ;;
    *) fail "MAGINE_INSTALL_DIR must be an absolute path" ;;
  esac
  case "$(uname -s)" in
    Darwin) os=darwin ;;
    Linux) os=linux ;;
    *) fail "this installer is for macOS and Linux. On Windows, run in PowerShell: irm https://magine.cloud/install.ps1 | iex" ;;
  esac
  case "$(uname -m)" in
    x86_64 | amd64) arch=x64 ;;
    arm64 | aarch64) arch=arm64 ;;
    *) fail "there is no Magine CLI build for $(uname -m) yet" ;;
  esac
  # An Intel shell under Rosetta on Apple silicon still gets the native build.
  if [ "$os" = darwin ] && [ "$arch" = x64 ] && [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || echo 0)" = 1 ]; then
    arch=arm64
  fi
  need curl
  need tar

  pkg="@magine/cli-$os-$arch"
  say "$(paint "$cat_c" '  /\_/\ ')  $(paint "$bold" 'Magine CLI installer')"
  say "$(paint "$cat_c" ' ( o.o )')  Looking up $pkg@$version"
  say "$(paint "$cat_c" '  > ^ < ')"
  meta="$(fetch "$registry/@magine%2fcli-$os-$arch/$version")" || fail "could not reach $registry (is $pkg@$version published?)"
  tarball="$(json_field tarball "$meta")"
  integrity="$(json_field integrity "$meta")"
  resolved="$(json_field version "$meta")"
  [ -n "$tarball" ] && [ -n "$integrity" ] || fail "the registry answered without a download for $pkg"
  case "$tarball" in
    "$registry"/*) ;;
    *) fail "the download is not on $registry; refusing it" ;;
  esac
  case "$integrity" in
    sha512-*) ;;
    *) fail "the registry gave no sha512 checksum for $pkg" ;;
  esac

  work="$(mktemp -d 2>/dev/null || mktemp -d -t magine)"
  trap 'rm -rf "$work"' EXIT
  trap 'exit 130' INT TERM
  say "$(paint "$dim" "Downloading magine $resolved for $os/$arch...")"
  fetch -o "$work/package.tgz" "$tarball" || fail "the download failed"
  expected="$(base64_to_hex "${integrity#sha512-}")"
  actual="$(sha512_hex "$work/package.tgz")"
  [ "$actual" = "$expected" ] || fail "the download does not match its checksum; nothing was installed"
  tar -xzf "$work/package.tgz" -C "$work"
  [ -f "$work/package/magine" ] || fail "the package has no magine binary"

  mkdir -p "$dir"
  cp "$work/package/magine" "$dir/.magine.new"
  chmod 755 "$dir/.magine.new"
  mv -f "$dir/.magine.new" "$dir/magine"
  installed="$("$dir/magine" --version 2>/dev/null || true)"
  [ -n "$installed" ] || fail "the installed binary does not run on this computer"

  on_path=yes
  case ":$PATH:" in
    *":$dir:"*) ;;
    *) on_path=no ;;
  esac
  if [ "$on_path" = no ] && [ "${MAGINE_NO_MODIFY_PATH:-}" != 1 ]; then
    line="export PATH=\"$dir:\$PATH\""
    zshrc="${ZDOTDIR:-$HOME}/.zshrc"
    fish_rc="${XDG_CONFIG_HOME:-$HOME/.config}/fish/config.fish"
    changed=""
    # The rc file of your login shell is created if missing (a new Mac has no ~/.zshrc).
    case "${SHELL:-}" in
      */zsh) [ -f "$zshrc" ] || : >"$zshrc" ;;
      */bash) [ -f "$HOME/.bashrc" ] || : >"$HOME/.bashrc" ;;
      */fish) mkdir -p "$(dirname "$fish_rc")" && { [ -f "$fish_rc" ] || : >"$fish_rc"; } ;;
      *) [ -f "$HOME/.profile" ] || : >"$HOME/.profile" ;;
    esac
    for rc in "$zshrc" "$HOME/.bashrc" "$HOME/.bash_profile" "$HOME/.profile"; do
      if [ -f "$rc" ] && ! grep -qF "$dir" "$rc"; then
        printf '\n# Magine CLI\n%s\n' "$line" >>"$rc"
        changed="$changed $rc"
      fi
    done
    if [ -f "$fish_rc" ] && ! grep -qF "$dir" "$fish_rc"; then
      printf '\n# Magine CLI\nfish_add_path "%s"\n' "$dir" >>"$fish_rc"
      changed="$changed $fish_rc"
    fi
    if [ -n "$changed" ]; then
      say "Added $dir to PATH in:$changed"
    fi
  fi

  say ""
  say "$(paint "$cat_c" '  /\_/\ ')  $(paint "$ok_c" "Magine CLI $installed is installed"): $dir/magine"
  say "$(paint "$cat_c" ' ( o.o )')  Next: $(paint "$bold" 'magine login')"
  say "$(paint "$cat_c" '  > ^ < ')  Docs: $(paint "$link_c" 'https://magine.cloud/docs#cli')"
  if [ "$on_path" = no ]; then
    say ""
    say "To use it in this terminal, run: export PATH=\"$dir:\$PATH\""
  fi
}

# Everything above only defines functions: a download cut short runs nothing.
main "$@"
