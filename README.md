# sc2remap

## Features

The grave key is redefined on-the-fly to always be equal to the most-recently pressed non-modifier key.
This entirely prevents needing to press a key multiple times, e.g. double tapping a control group to
move the camera to the control group, or making pairs of units out of a Terran production facility with
a reactor attached.

PgUp/PgDown are emitted when scrolling up/down respectively. SC2 disallows binding the scroll wheel to
certain actions for historical reasons. Rapid fire via keyboard repeat is much faster than one could
possibly scroll anyway, so there's no point in preventing binding of the scroll wheel. Note that the
scrolling inputs are not actually replaced, and will still be sent (meaning that using it to adjust the
amount of resources to send to team mates in team games should not be affected, although this has not
been tested).
  
## Mechanism

On program startup, events from all evdev devices (in reality the devices numbered 1-100, but systems
shouldn't really have more than 100 input devices) are read to determine the keyboard and mouse devices.
The heuristic is that a mouse device generates events corresponding to the 5 main buttons, scrollwheel,
and REL events; and a keyboard device generates any KEY event that is not one of the 5 mouse buttons or
the scrollwheel. This heuristic is not perfect, but good enough for most purposes.

The keyboard device is then grabbed (meaning that its inputs are no longer visible to any other
application in the system); the mouse device is not.

A uinput device is created, with all KEY and REL events enabled (for whatever reason, in order for a
uinput device to be able to send mouse button KEY events, the REL event code must be enabled).

Events from the keyboard and mouse evdev devices are read in the main loop. New events may be injected,
and the read events may be modified or forwarded without modification to implement the desired
functionality.
 
## Known Bugs

- [ ] It appears that grabbing the keyboard device while a key is down can result in the key-release event
    never being observed, causing the key to be held down, and requiring another press of the key to
    resolve the issue. This is somewhat mitigated against by the logic which determines which device is
    the keyboard only attempting to grab the device on a key-release event, but this is not perfect as
    multiple keys could be held down.
- [ ] There's probably a better way to enumerate all evdev devices rather than just trying all IDs from 1
    to 100.
