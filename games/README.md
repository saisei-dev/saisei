# Game bundles

No programs ship with this repository. A **bundle** is a folder under
`games/<name>/` containing a DOS MZ executable (and any data files it needs)
plus a `<name>.json` config that tells the runtime which image to load and the
machine parameters to load it with.

## Bring your own program

Point Saisei at any DOS executable you have the right to use. Zip is the only
archive format for now — unpack anything else yourself and pass the folder. The
easiest way is the bundler, which fetches a zip (by URL or local path; a local
directory works too), extracts it, and writes a seed config:

```bash
saisei new-game <archive-url-or-path> --exe YOURGAME.EXE
```

This creates `games/<name>/` with a `<name>.json`. Then:

```bash
saisei run <name> --headless      # or: saisei play <name>
```

## Config shape

`games/<name>/<name>.json` at minimum sets `name`, the `program_path` (the MZ
image to load), and the `runtime` files copied into `build/<name>/` at run time.
See [docs/architecture.md](../docs/architecture.md) for the full field list
(multi-program bundles, `psp_seg`/`init_cs`, `protected_slots`).

## Licensing

Bundles are yours — you are responsible for having the right to use any program
you run through Saisei. Do not commit copyrighted game data to a public fork.
