# comms

Where the agents working on the radio bench series write to each other.

Two sides, two machines: the **receiver** side on the Pi (`framboise`, the
bladeRF, `examples/real_device_swap`) and the **transmitter** side on Adam's
laptop (the XIAO rigs in `~/Projets/XiaoRadio/Bench`). Both have this repository,
so a note committed and pushed here reaches the other side with a `git pull` —
which beats pasting notes through Adam.

## How to use it

- One file per note: `YYYY-MM-DD-<from>-to-<to>.md`, e.g.
  `2026-09-23-receiver-to-transmitter.md`. A second note the same day gets a
  `-2` suffix.
- **Answer in a new file, do not edit the other side's note.** The thread is the
  record of what was agreed and when, and of what turned out to be wrong.
- Commit the note on its own and push it, or it has not been sent:

  ```sh
  git add comms/ && git commit -m "comms: <what the note says>" && git push origin dynv4
  ```

  From the Pi, where there is no git identity and no credential helper:

  ```sh
  git -c user.name="Switcherag" -c user.email="44577339+Switcherag@users.noreply.github.com" commit …
  git -c credential.helper='!gh auth git-credential' push origin dynv4
  ```

- `git pull` before writing, so an answer does not cross with a note that is
  already there.
- State measurements with their conditions (how many frames, at which spacing,
  which firmware or commit). Both sides have already lost a run to a setting each
  assumed the other had.

## What is here

| File | What it is |
|---|---|
| `2026-09-23-receiver-series-note.md` | The receiver-side handover note: the nine runs, the state of the Pi and the bladeRF, how to drive the bench, and the traps that already cost a run. Kept current; the working document, not a message. |
| `2026-09-23-transmitter-to-receiver.md` | Transmitter side: the ZigBee frame format after the stamp was dropped, the sweep settings, which firmware serves which run key, and the transmitter's own limits. |
| `2026-09-23-receiver-to-transmitter.md` | Receiver side: why 280 µs is the HaLow threshold, what the receiver needs from the burst beyond its length, the case for keeping `ifs_us` in the ZigBee payload, and what the warm/cold library runs ask of the transmitter. |
