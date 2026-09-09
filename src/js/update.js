"use strict";
window.Corvus = window.Corvus || {};

/*
  Corvus.update — "a newer release exists" dialog.

  Backed entirely by GET /api/update, which compares the running version
  against the GitHub releases of the project. There is no update server and
  nothing here knows a version literal: both the installed and the available
  version come from that endpoint.

  Three rules keep this out of the operator's way in the field, where the
  laptop has no internet and a flight is in progress:

    * the check runs in the background AUTO_DELAY_MS after load, so nothing
      on screen ever waits for it, and a failure is silent;
    * the dialog is raised once per release — "Skip this version" persists to
      the config, and a version already dismissed never prompts again;
    * it is never raised while the vehicle is armed. An update notice over a
      flying aircraft is worse than no notice at all; the check is retried
      once the vehicle disarms.

  The release page is opened through POST /api/update/open rather than a
  link, because the desktop build runs inside QtWebEngine where an external
  link goes nowhere. "Copy link" is the fallback when no browser opens.
*/
Corvus.update = (function () {
  /* Long enough that the map, telemetry stream and panels have settled — the
     dialog must never be the first thing that paints. */
  const AUTO_DELAY_MS = 4000;
  /* Release notes are a summary here, not a changelog viewer. */
  const NOTES_MAX_LINES = 12;

  let dialog = null;        // Corvus.ui.modal handle while the dialog is open
  let armedUnsubscribe = null;   // telemetry subscription while we wait to disarm
  let pending = null;       // status held back because the vehicle was armed

  function request(refresh) {
    return Corvus.telemetry.requestJson("/api/update" + (refresh ? "?refresh=1" : ""));
  }

  /** True while the vehicle is armed — the one state that suppresses the dialog. */
  function isArmed() {
    const state = Corvus.telemetry.getState();
    return !!(state && state.armed);
  }

  /** ISO timestamp -> the operator's locale date, or "" when unparsable. */
  function publishedDate(iso) {
    if (!iso) return "";
    const when = new Date(iso);
    if (isNaN(when.getTime())) return "";
    return when.toLocaleDateString();
  }

  /* Markdown headings/bullets are left as typed: GitHub release bodies are
     short and the raw text reads fine, while rendering markdown here would
     mean trusting network content as markup. */
  function notesText(notes) {
    const lines = String(notes || "").split("\n").filter((l) => l.trim() !== "");
    if (!lines.length) return "";
    if (lines.length <= NOTES_MAX_LINES) return lines.join("\n");
    return lines.slice(0, NOTES_MAX_LINES).join("\n") + "\n…";
  }

  function copyLink(url) {
    try {
      if (navigator.clipboard && navigator.clipboard.writeText) {
        return navigator.clipboard.writeText(url);
      }
    } catch (_error) { /* fall through to the textarea path */ }
    /* QtWebEngine without clipboard permission still honours execCommand. */
    return new Promise((resolve, reject) => {
      const box = document.createElement("textarea");
      box.value = url;
      box.setAttribute("readonly", "readonly");
      box.style.position = "fixed";
      box.style.opacity = "0";
      document.body.appendChild(box);
      box.select();
      let ok = false;
      try { ok = document.execCommand("copy"); } catch (_error) { ok = false; }
      document.body.removeChild(box);
      if (ok) resolve(); else reject(new Error("copy failed"));
    });
  }

  function buildBody(status) {
    const frag = document.createDocumentFragment();

    const lead = document.createElement("div");
    lead.className = "page-card-desc";
    lead.textContent =
      "A newer version of Corvus GCS has been published. Updating is manual: " +
      "download the release for this platform and replace the installed app.";
    frag.appendChild(lead);

    const rows = document.createElement("div");
    rows.appendChild(Corvus.ui.row("Installed", status.current || "—"));
    rows.appendChild(Corvus.ui.row("Available", status.latest || "—"));
    const published = publishedDate(status.published);
    if (published) rows.appendChild(Corvus.ui.row("Published", published));
    frag.appendChild(rows);

    const notes = notesText(status.notes);
    if (notes) {
      frag.appendChild(Corvus.ui.label("Release notes"));
      const pre = document.createElement("pre");
      pre.className = "update-notes";
      pre.textContent = notes;
      frag.appendChild(pre);
    }

    /* Printed as text, not a link: in the desktop build a link is inert, so
       the URL has to be readable (and copyable) on its own. */
    frag.appendChild(Corvus.ui.label("Release page"));
    const url = document.createElement("div");
    url.className = "update-url";
    url.textContent = status.url || "";
    frag.appendChild(url);

    const note = Corvus.ui.message({ className: "update-msg" });
    frag.appendChild(note.el);

    return { el: frag, note };
  }

  /** Raise the dialog for *status*. Returns the modal handle. */
  function show(status) {
    if (dialog) dialog.close();
    const built = buildBody(status);

    const openBtn = Corvus.ui.button({
      variant: "primary",
      icon: "download",
      label: "Open release",
      onClick: () => {
        Corvus.ui.setBusy(openBtn, true);
        Corvus.telemetry.requestJson("/api/update/open", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: "{}",
        }).then((res) => {
          if (res && res.ok) {
            built.note.show("Opened in your browser.", "ok");
            return;
          }
          /* No browser on the box (or none the backend could launch): the
             link is still useful, so put it on the clipboard instead. */
          return copyLink(status.url).then(
            () => built.note.show("No browser available — link copied to the clipboard.", "warn"),
            () => built.note.show("No browser available. The release page is shown above.", "warn"),
          );
        }).catch(() => {
          built.note.show("Could not open the release page.", "err");
        }).finally(() => {
          Corvus.ui.setBusy(openBtn, false);
        });
      },
    });

    const copyBtn = Corvus.ui.button({
      variant: "secondary",
      icon: "clipboard",
      label: "Copy link",
      onClick: () => copyLink(status.url).then(
        () => built.note.show("Link copied to the clipboard.", "ok"),
        () => built.note.show("Could not copy — the link is shown above.", "warn"),
      ),
    });

    const skipBtn = Corvus.ui.button({
      variant: "secondary",
      label: "Skip this version",
      title: "Do not prompt again for " + (status.latest || "this release"),
      onClick: () => {
        /* Fire-and-forget: the dialog closes either way, because a failed
           write must not trap the operator in a dialog they dismissed. */
        Corvus.telemetry.postAction("/api/update/skip", { version: status.latest })
          .catch(() => {});
        dialog.close();
      },
    });

    const laterBtn = Corvus.ui.button({
      variant: "secondary",
      label: "Later",
      onClick: () => dialog.close(),
    });

    dialog = Corvus.ui.modal({
      title: "Update available",
      size: "lg",
      body: built.el,
      /* One pre-built group rather than four loose buttons: the modal stretches
         whatever it is handed to equal widths, and four equal columns in this
         dialog wraps every label onto three lines. Nesting a plain .ui-actions
         inside lets the buttons keep their natural width. */
      actions: Corvus.ui.actions([openBtn, copyBtn, skipBtn, laterBtn]),
      onClose: () => { dialog = null; },
    });
    dialog.open();
    return dialog;
  }

  /* Hold a pending notice until the vehicle disarms, then raise it. One
     subscription at a time; dropped as soon as it fires. */
  function showWhenDisarmed(status) {
    pending = status;
    if (armedUnsubscribe) return;
    armedUnsubscribe = Corvus.telemetry.subscribe((state) => {
      if (state && state.armed) return;
      const held = pending;
      pending = null;
      if (armedUnsubscribe) { armedUnsubscribe(); armedUnsubscribe = null; }
      if (held) show(held);
    });
  }

  /**
   * Ask the backend whether a newer release exists.
   *
   * `opts.refresh` forces a network check (the Settings page's "Check now");
   * without it the backend answers from its cache. `opts.manual` means the
   * operator asked, so the dialog is raised even for a version they skipped
   * and the promise is allowed to reject so the caller can report the error.
   * The automatic call swallows everything — an unreachable GitHub is the
   * normal state in the field, not something to tell the operator about.
   *
   * Resolves with the status object (or null when the automatic check failed).
   */
  function check(opts) {
    const o = opts || {};
    return request(!!o.refresh).then((status) => {
      if (!status) return null;
      if (!status.update_available) return status;
      const skipped = !o.manual && status.skipped && status.skipped === status.latest;
      if (skipped) return status;
      if (isArmed()) showWhenDisarmed(status);
      else show(status);
      return status;
    }).catch((error) => {
      if (o.manual) throw error;
      return null;
    });
  }

  /** Schedule the background check. Safe to call when the backend is down. */
  function init() {
    window.setTimeout(() => { check(); }, AUTO_DELAY_MS);
  }

  return { init, check, show, close: () => { if (dialog) dialog.close(); } };
})();
