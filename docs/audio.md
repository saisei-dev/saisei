# Sound

Everything under `runtime/src/audio/`. Four emulated cards, one mixer, one SDL
device — and one deliberate departure from fidelity, argued for below.

| | what it is | where |
|---|---|---|
| **OPL2 / YM3812** | AdLib. 9 channels x 2 operators of FM. Also *is* the FM half of a Sound Blaster, at 0x228/0x229 as well as 0x388/0x389. | `opl2.rs` |
| **PC speaker** | PIT channel 2 and port 61h. Re-voiced (see below). | `speaker.rs` |
| **SN76489** | The Tandy 1000 / PCjr three-voice chip, port 0xC0. | `sn76489.rs` |
| **Sound Blaster** | The digitised half: a DSP at 0x220-0x22F fed by DMA. | `sb.rs`, `dma.rs` |

`mod.rs` is the mixer; `dsp.rs` is the shared signal blocks (envelopes, filters,
reverb); `out.rs` is the SDL sink.

---

## Where samples come from

Every source renders **on the guest thread, in virtual time**. The virtual clock
is instruction-driven (`timer.rs`), so "how much audio does this interval owe" is
just `elapsed_virtual_ns * rate`, and the mixer catches up to
`shim_virtual_now_ns()` whenever asked. There are two kinds of catch-up:

* **Forced** — every write to a sound port calls `audio::catchup()` *before* the
  write lands, so the samples for the interval that just elapsed are rendered
  against the *old* register state and the write takes effect at its own virtual
  timestamp. This is not a nicety: a PC-speaker PWM driver toggles the gate at
  several kHz, and quantising those edges to a service tick turns digitised
  speech into noise.
* **Periodic** — `safe_point_impl` calls `audio::service()` about every
  millisecond of virtual time, so a game holding a note without touching a port
  still gets its samples produced.

There is no audio thread and no lock. `SDL_QueueAudio` is a push API; a callback
device would mean running the mixer on SDL's thread, which would mean every synth
reading guest state (the OPL2 register file, the PIT divisor, the DMA window)
across a thread boundary.

## Keeping the queue fed

This is where the bodies are buried. Three things were each independently enough
to make the sound crackle, and all three are non-obvious:

1. **The queue must be primed.** We only ever render the audio virtual time has
   *earned* — one guest-second is one second of samples — so there is nothing
   spare to build a buffer out of. Left alone, the queue starts empty and
   random-walks around empty, and every frame present or pacing sleep punches
   through the floor. `prime()` gives it a cushion; the rate controller then
   defends it. (A rate controller cannot *create* one: at a few percent of
   authority, filling 110ms from empty takes seconds.)

2. **Audio must render before the pacer sleeps and before the frame presents.**
   Both cost real time in which *no virtual time passes* — so they produce no
   samples and only drain the queue. Rendering after them (which is where it
   first sat) meant every safepoint topped the queue up and then immediately let
   them eat it, with nothing to give back.

3. **The rate controller must act on the timescale of the thing it corrects.**
   The queue holds a tenth of a second and the error is a slow drift. Tuned to
   re-evaluate every 10ms with an integral that could swing 40%/second, it did
   not correct the drift — it *became* the drift, hunting between overfull and
   empty (`10666 -> 1024 -> 8763 -> 325 frames`) and cracking on every downswing.
   It now runs at 20Hz with gentle gains.

### The guest's speed is fed forward, not chased

**The emulated machine does not run at 1.00x.** In a heavy scene it drops —
Zeliard sits at **0.82x** coming out of a shop and stays there. One virtual second
then buys only 0.82 seconds of samples while the device keeps eating a full second
of them, and two things follow which are really the same bug:

* the queue bleeds dry, and the sound crackles; and
* the chips advance **one sample per output frame**, so they run at 0.82x of their
  nominal rate in real time — and the music comes out about two semitones flat.
  That is what "stretched" sounds like.

The correction for both is the same number, `1 / speed`, and that is not a
coincidence: the ratio that holds the queue level is exactly the ratio that makes
a chip advance at its nominal rate in real time. **Rate-correcting a slow guest
does not detune it — failing to correct it is what detunes it.** The naive fear
has the sign backwards.

So `GUEST_SPEED` is measured directly and fed forward. An earlier version watched
only the queue, which meant it had to *discover* an 18% shortfall by first running
dry and then chase it with an integral clamped at +10% — it could never reach the
+22% needed, so it pinned at its limit and the queue simply stayed empty. The PI
loop survives, demoted to what it is actually good at: trimming the residual (the
device's clock is not exactly 48000Hz either) and holding the queue at target.

What this **cannot** fix is tempo. At 0.82x the music driver's own ticks happen in
guest time, so the song plays at 82% speed. Pitch and dropouts are the mixer's to
solve; a slow machine playing its song slowly is the machine being slow, and the
place to fix that is the machine.

**Known limit.** While the JIT compiles a chunk the guest is stopped for 1-2
seconds, and no buffer covers that: the audio goes silent and re-anchors. It
happens only on first encounter with new code; a warm cache is clean.

---

## The PC speaker, and why it does not sound like a PC speaker

This is the one place the runtime deliberately does not reproduce the hardware's
*sound*, so it deserves a straight answer rather than a footnote.

**The guest still sees a real 8254 and a real port 61h.** The gate, the
speaker-data bit, channel 2's mode and divisor, and the output pin the guest
reads back through port 0x61 bit 5 all keep their exact hardware semantics, in
`shims.rs`. `speaker.rs` only *reads* them. In fact the audio path and the
guest's port-61h reads go through the same function — `pit_ch2_output_at()` — so
the sound a game makes and the bit it reads back cannot disagree.

The prime directive ("emulate faithfully — no heuristic harnesses") is about the
machine the program runs on. What we do with the resulting pin waveform on its way
to the speakers is a rendering choice, downstream of the emulation, and it changes
nothing the guest can observe or branch on. Nothing here can make a wrong
behaviour masquerade as working, because nothing here is visible to the program.

### Two things a game does with one pin

Prince of Persia does both at once — its SETUP.CFG offers "Standard PC Internal
Speaker" for MIDI *and* for DIGITAL:

* **Tones.** The CPU programs channel 2's divisor once and leaves the gate open.
  The pitch is `1193182 / divisor` and the *hardware* generates the waveform.
  This is music: note data, played on a square-wave oscillator.
* **PWM.** The CPU carries the waveform itself, toggling the speaker-data bit
  thousands of times a second. This is digitised sound, and the divisor means
  nothing.

Tones are **re-voiced**: the note stream is extracted and played through a soft
synth (band-limited oscillator, ADSR, lowpass, delayed vibrato, a small stereo
reverb, and a voice pool so release tails ring under the note that replaced them).
PWM cannot be turned into notes — there are no notes — so it is reproduced
faithfully as PCM, box-filtered against aliasing, DC-blocked and lowpassed.

### Telling them apart without guessing

The distinction is not a heuristic about what the game "meant". It is structural:
**PWM must modulate something at the sample rate, and a held note must not.** So
the test is stability — has this exact `(gate, enable, mode, divisor)` tuple been
left alone long enough to *be* a note?

Answering that at the moment of the write would require predicting the future, and
guessing is exactly what this codebase refuses to do. So it doesn't: **the speaker
renders 3ms behind the rest of the mixer.** Writes land in a timestamped queue,
and by the time the renderer reaches a segment it can already see what came after
it and *knows* whether the tuple survived. A note is recognised because it was
held; a PWM step is recognised because the next write was already on its way. No
prediction, no detector, no fallback — just a decision made late enough to be
certain. The cost is 3ms of latency on the speaker alone.

The raw pin is fed to the output **only** on an unstable segment. A stable segment
is either a note (the synth has it) or a parked pin level — which on real hardware
makes no sound once the cone has settled, and contributes only the click of
getting there. That is a click we are specifically here not to reproduce, and
keying the pin path off *stability* rather than off "is there a note" is also what
keeps note edges clean: silence and notes are both stable, so the pin path is
already muted before a note starts and never leaks a slice of square into its head
or tail.

---

## Fidelity notes

**OPL2.** Built from the YM3812 hardware documentation: log-sin and exp tables
generated from their closed forms, 9x2 operators, the 15-row envelope increment
table with its rate-group shifts, the exponential attack, key-scale level and
rate, the four waveforms (gated on the waveform-select enable in reg 0x01, which
early titles leave off), feedback as the average of an operator's last two
outputs, both connection modes, the 3.7Hz tremolo and 6.07Hz vibrato LFOs, and
rhythm mode including the noise LFSR and the hi-hat/snare/cymbal phase mangling.

Two details are genuinely disputed between the die-level references and are
called out where they are implemented: the cymbal's phase constant (0x80 vs
0x100) and one term of `rm_xor`. Both affect only the timbre of two percussion
voices.

**Snapshots.** `Opl2State` (the register file) is FROZEN — snapshots serialise it
byte-for-byte and it may not gain a field. All synth state is *derived*, lives in
separate statics for exactly that reason, and is re-derived from the restored
register file on load. The SN76489 and Sound Blaster are not in the snapshot at
all and reset on restore (a one-sound transient, the same concession
`pit_ch2_mode` already makes).

**io_bus and `exit(1)`.** An unclaimed port does not get ignored on this bus — it
reaches `io_port_error`, which calls `exit(1)`. So a device must claim its *whole*
decode range, including the registers it does not model, and answer them the way
an absent register does. Before this work, touching almost any DMA port killed the
process.

---

## Volume

Per game, because loudness is a property of the game and not of the player: a
beeper game and an AdLib game mastered a decade apart do not arrive at the same
level, and a volume set once for one of them is the wrong volume for the next.

The slider is in the overlay's **Settings** page (F12 -> Settings), which shares
the overlay's panel. Drag it, click it, roll the wheel, or use Left/Right. It is
saved to `$XDG_DATA_HOME/saisei/settings.json`:

```json
{ "default_volume": 0.6, "volume": { "zeliard_dos_en": 0.35 } }
```

Missing, unreadable and corrupt all mean the same thing — defaults. A settings
file is never worth failing to start a game over.

Moving the slider plays a short reference tone (`saisei_audio_preview`). The guest
is frozen while the overlay is up, so without it you would be setting a level you
could not hear until you closed the menu, which is not a volume control but a
guess.
