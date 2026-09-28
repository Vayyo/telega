#!/bin/sh
# Installs the built client as a desktop application of the current user:
#
#   ~/.local/bin/telega                          symlink to target/release/telega
#   ~/.local/share/icons/hicolor/.../apps/telega icon (SVG + PNG sizes)
#   ~/.local/share/applications/telega.desktop   launcher entry
#
# After that the client is found by an XDG launcher (for example,
# `gtk-launch telega`). Run with `--uninstall` to remove the entries again.
#
# The release binary carries RUNPATH .../third_party/tdlib/lib, so the
# repository must stay where it is for the symlink to keep working.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
data=${XDG_DATA_HOME:-$HOME/.local/share}
bin_dir=$HOME/.local/bin
name=telega

binary=$root/target/release/$name
icons=$data/icons/hicolor
entry=$data/applications/$name.desktop

case ${1:-} in
--uninstall)
    rm -f "$bin_dir/$name" "$entry" "$icons/scalable/apps/$name.svg"
    for size in 32 48 64 128 256; do
        rm -f "$icons/${size}x${size}/apps/$name.png"
    done
    update-desktop-database "$data/applications" >/dev/null 2>&1 || true
    gtk-update-icon-cache -f -t "$icons" >/dev/null 2>&1 || true
    echo "убрано: $bin_dir/$name, $entry, значки в $icons"
    exit 0
    ;;
"") ;;
*)
    echo "использование: $0 [--uninstall]" >&2
    exit 2
    ;;
esac

if [ ! -x "$binary" ]; then
    echo "нет $binary — сначала соберите клиент (cargo build --release)" >&2
    exit 1
fi

mkdir -p "$bin_dir" "$data/applications" "$icons/scalable/apps"
ln -sfn "$binary" "$bin_dir/$name"

install -m 644 "$root/assets/$name.svg" "$icons/scalable/apps/$name.svg"

# PNGs for launchers that do not read SVG. Any of the three converters will
# do; without one the SVG is still installed.
renderer=""
for candidate in rsvg-convert magick convert; do
    if command -v "$candidate" >/dev/null 2>&1; then
        renderer=$candidate
        break
    fi
done
sizes=""
if [ -n "$renderer" ]; then
    for size in 32 48 64 128 256; do
        dir=$icons/${size}x${size}/apps
        mkdir -p "$dir"
        case $renderer in
        rsvg-convert)
            rsvg-convert -w "$size" -h "$size" "$root/assets/$name.svg" -o "$dir/$name.png"
            ;;
        *) # magick / convert, the ImageMagick CLI
            "$renderer" -background none -density 384 "$root/assets/$name.svg" \
                -resize "${size}x${size}" "$dir/$name.png"
            ;;
        esac
    done
    sizes=", PNG 32–256"
else
    echo "предупреждение: rsvg-convert/magick/convert не найдены, только SVG" >&2
fi

# The launcher keeps no PATH of its own: the entry points at the symlink by
# absolute path.
sed "s|@EXEC@|$bin_dir/$name|g" "$root/packaging/$name.desktop" >"$entry.new"
chmod 644 "$entry.new"
mv "$entry.new" "$entry"

update-desktop-database "$data/applications" >/dev/null 2>&1 || true
gtk-update-icon-cache -f -t "$icons" >/dev/null 2>&1 || true

echo "установлено: $bin_dir/$name -> $binary"
echo "установлено: $entry (Exec=$bin_dir/$name)"
echo "установлено: значок $icons/scalable/apps/$name.svg$sizes"
