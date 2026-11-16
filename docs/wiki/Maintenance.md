# Maintenance

## Swap

`swap = "zram"` (the default) makes compressed swap in memory, half the size of your RAM, which holds roughly twice that
after compression. There is no swap file or partition, so hibernation is not supported. `swap = "none"` turns it off.

## Routine tasks

A background service looks once an hour for work that is due and runs it:

| Task | How often | What it does |
|---|---|---|
| `trim` | weekly | `fstrim`: tells the SSD which blocks are free |
| `prune` | weekly | removes old generations beyond `keep_generations` |
| `gc` | weekly | deletes packages nothing uses any more |
| `scrub` | monthly | reads every block of the disk and checks its checksum |

On a new system the first look only starts each task's clock; nothing runs at the first boot.

```toml
[maintenance]
enable           = true
keep_generations = 10
```

```bash
forge maintenance status         # when each task last ran and when it is due
sudo forge maintenance run       # run everything now
sudo forge maintenance run trim  # run one task now
```

The booted generation and the newest ones are never pruned.
