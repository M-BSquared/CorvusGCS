# Interface languages

One JSON file per language, named after its code: `en.json`, `de.json`,
`fr.json`, `dl.json` (Drachensprech). Corvus lists every file in this folder under Settings, Interface,
Language, and in the first start setup. Adding a language means adding a file
here. No code changes.

An operator who only has the installed app can drop the same kind of file into
`~/.corvus/languages` instead. It survives updates, and a file there with the
code of a language that ships with Corvus replaces it. Press Reload languages
in Settings after copying one in.

## The format

```json
{
  "meta": {
    "name": "German",
    "native_name": "Deutsch"
  },
  "strings": {
    "Settings": "Einstellungen",
    "Settings of {name}": "Einstellungen von {name}"
  }
}
```

* `meta.name` is the language in English, `meta.native_name` in itself. The
  dropdown shows "Deutsch (German)".
* `strings` maps the **English text of the interface**, copied exactly, to its
  translation. There are no message ids. An English text a file does not list
  stays English, so a file can be started small and grown.
* Whitespace and line breaks in the English text do not matter, but capital
  letters and punctuation do: `"ARM"` and `"Arm"` are two texts.
* `{name}` in an English text matches whatever stands in that place, and the
  same `{name}` in the translation puts it back. What it matched is translated
  too, when the file has an entry for it. Both sides must use the same names.
* A label that ends in an autopilot parameter, such as `Action on RC loss
  (NAV_RCL_ACT)`, is translated by the label alone: `"Action on RC loss"`.
* An empty translation counts as not translated.
* Write plain prose: no em dash, no en dash, and no hyphen standing between
  words as a dash. Write number ranges with the word for "to".
* `meta.last` set to `true` sorts the language to the end of the list, after the
  others. Without it, languages are listed by name.
* The file name is the code (`pt-BR.json`). A `code` inside `meta` is ignored.

## Starting a language

In a checkout, this writes a file with every text the other languages already
list, each blank:

```bash
python3 tools/i18n.py new es Spanish Español
```

Fill in the blanks. Leave a text blank, or delete the line, where the English is
fine as it is (`PX4`, `GPS`).

To find texts nobody has translated, open Corvus in the language, go through the
pages, and run this in the browser console. It prints the English texts that
were on screen without a translation, each with a blank:

```js
copy(JSON.stringify(Corvus.i18n.template(), null, 2))
```

Check the result before you send it:

```bash
python3 tools/i18n.py check
```

It reads every file and reports a text whose `{placeholders}` differ from the
translation, a translation with a dash in it, and how many known texts a
language lacks.

## Plugins

A plugin keeps its own translations in its own folder, the way it keeps its own
tests: `plugins/<id>/lang/<code>.json`, with the same `strings` map (`meta` is
optional there, the main file names the language). When a language is loaded,
the files of every bundled and installed plugin are merged into it. If the main
file and a plugin translate the same English text, the main file wins. Texts
that only one plugin shows belong in that plugin's folder, not here.

`python3 tools/i18n.py check` reads the plugin files as well and compares each
plugin's languages with one another.

## Keeping buttons short

Some places have no room: the left rail (`HOME`, `SETUP`), the top bar captions
and the flight bar (`ARM`, `TAKEOFF`). Choose short words there, as the shipped
files do.
