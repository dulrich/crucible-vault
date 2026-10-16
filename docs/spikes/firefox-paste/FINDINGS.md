# Firefox "Copy Image" clipboard targets (2026-10-15)

Session: X11 (i3), xclip at /usr/bin/xclip. User ran Firefox "Copy Image"; only target names and sizes recorded, no content.

Commands:
- `xclip -selection clipboard -t TARGETS -o` → TIMESTAMP, TARGETS, MULTIPLE, SAVE_TARGETS, image/png, image/jpeg, image/bmp, image/x-bmp, image/x-MS-bmp, image/x-icon, image/x-ico, image/x-win-bitmap, image/vnd.microsoft.icon, application/ico, image/ico, image/icon, text/ico, image/tiff, image/webp, audio/x-riff (exit 0)
- `xclip -selection clipboard -t image/png -o | wc -c` → 180528 bytes; first 8 bytes = PNG signature `89 50 4e 47 0d 0a 1a 0a`
- `xclip -selection clipboard -t text/html -o | wc -c` → 0 (not offered)

Conclusion: Firefox offers raw image bytes in many GTK-converted formats and no text/html or text/plain. WP-7 reads `image/png` first (always offered via GTK conversion), saves as `.png`; the HTML-`<img src>` branch is not exercised by Firefox "Copy Image" and is kept only as a fallback for sources offering HTML alone.
