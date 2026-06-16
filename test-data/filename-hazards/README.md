# Filename hazard samples

A folder of deliberately deceptive filenames for exercising Feraille's
filename hazard detection — the kind of tricks malware and phishing use to
make a file's name lie about what it is. The detection lives in
[`feraille-core/src/name_hazards.rs`](../../crates/feraille-core/src/name_hazards.rs);
Get Info (Cmd+I) and the preview pane highlight each flagged character with a
tooltip, and show an invisible character via a visible stand-in.

## Use

```sh
python3 test-data/filename-hazards/generate.py
```

This writes the files into `samples/`. Open that folder in Feraille and select
each file: clean names render normally, deceptive ones light up
(amber = whitespace tricks, red = reordering / invisible / look-alike).

`samples/` is git-ignored — the names contain control and bidi characters that
git and editors render unpredictably, so we keep the generator (reviewable
plain text) under version control and let each machine materialize the files.

## What each sample demonstrates

| Sample | Hazard |
| --- | --- |
| `clean_invoice.pdf` | none (control) |
| `quarterly report v2.txt` | none — interior spaces are normal |
| ` leading-space.txt` | leading whitespace |
| `trailing-space.txt ` | trailing whitespace |
| `tab⇥inside.txt` | TAB posing as a space |
| `no break space.txt` | non-breaking spaces |
| `zero​width​split.exe` | zero-width spaces |
| `word⁠joiner.dll` | word joiner |
| `statement‮gpj.exe` | RLO bidi override (shows as `statementexe.jpg`) |
| `раypal-login.exe` | Cyrillic homoglyphs (`р`, `а`) |
| `gοοgle-update.exe` | Greek homoglyphs (`ο`) |
| `ｆｕｌｌwidth.exe` | fullwidth Latin letters |
| `café_menu.txt` | combining accent vs. precomposed `é` |
| `alertbell.log` | embedded BEL control character |
| `invoice​_рeal‭_final.exe` | several at once |
