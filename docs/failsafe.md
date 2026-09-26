# Emergency off switch: when the cursor won't release

If the pen keeps drawing after you lift it, or a held key/modifier stays
stuck, use **any one** of these. They are independent of each other, so a
single wedged component cannot block all of them.

## 1. PC panic hotkey (fastest): `Ctrl+Alt+Shift+F12`

Press it anywhere, even while drawing. It instantly lifts the pen, leaves
range, releases every held key and mouse button, and pauses pen input.
Hammer it — it is idempotent. Resume from the tray when calm.

- Change it in `config.json` via `panic_hotkey` (same `keys` grammar as
  buttons, e.g. `"ctrl+alt+p"`). An invalid value keeps the default and
  logs a warning.
- Implemented as an OS-level hook on its own thread (`RegisterHotKey` on
  Windows, `XGrabKey` on X11), outside the session tasks, touching neither
  network nor disk. On Wayland without the GlobalShortcuts portal, use the
  tray item instead — the UI says so.

## 2. Tray: Emergency release / Pause pen input

Same effect as the hotkey, plus a manual pause toggle. While paused, pen
records are dropped (one transitional `leave`), **buttons keep working**,
and a banner stays up until you resume.

## 3. Tablet: 3-finger tap on the strip

Stops the tablet's UDP sender first, then closes the session with `bye`.
No auto-reconnect for 5 s. Even if the Wi-Fi is dead and none of this
arrives, the PC still recovers: the pen watchdog lifts the cursor within
750 ms and dead-connection teardown releases keys within 5 s.

## 4. Automatic: stuck-pen auto-pause

If the pen watchdog has to repair a stuck pen 3 times in a row with no
clean input between, the companion auto-pauses pen input and banners
"Pen auto-paused after repeated stuck input." This catches silent UDP
blackholes (AT-8 style loss) without you noticing the mechanics. Resume
from the tray; the counter resets on any clean stroke.

## What to check afterward

1. Draw a short stroke and lift: it must end crisply (else suspect
   sustained >10% UDP loss — see the user guide's Wi-Fi notes).
2. Tap each hold button once: no modifier may remain down (type in a text
   field to confirm).
3. If it recurs on one network only, prefer 5 GHz Wi-Fi with the PC on
   Ethernet, and re-check the firewall rules (spec 9/10).
