"use strict";
window.Corvus = window.Corvus || {};

/**
 * Corvus.checklistEditor — the dialog the preflight checklists are written in.
 *
 * Opened from Settings > Appearance > Preflight checklist. Add, rename,
 * reorder, split into sections, delete. The lists themselves and the Home
 * window's state belong to Corvus.checklist; this dialog only edits them
 * through it.
 *
 * Two views, never both, like the Video page: the LIST of checklists, and
 * the EDITOR for one. Nothing is written until Save, so Cancel really
 * cancels.
 *
 * In the editor, Enter in an item adds the next one below it and Backspace
 * in an empty one removes it, so a list can be typed straight down without
 * reaching for the mouse.
 *
 * Exposes open() -> the dialog handle.
 */
Corvus.checklistEditor = (function () {
  const S = Corvus.setupShared;

  /**
   * The lists after saving `draft` into `all`: replaced in place when it is
   * already there, appended when it is new. Empty items are dropped.
   *
   * Pure, and exported for the test suite.
   *
   * @param {Array} all the lists as they are
   * @param {Object} draft {id, name, items}
   * @returns {Array}
   */
  function withDraft(all, draft) {
    const clean = {
      id: draft.id,
      name: String(draft.name || "").trim(),
      items: (draft.items || [])
        .map((it) => ({ text: String(it.text || "").trim(), heading: it.heading === true }))
        .filter((it) => it.text),
    };
    const out = (all || []).map((l) => (l.id === clean.id ? clean : l));
    if (!out.some((l) => l.id === clean.id)) out.push(clean);
    return out;
  }

  /**
   * Why a draft cannot be saved yet, or "" when it can.
   * @param {Object} draft
   * @returns {string}
   */
  function draftProblem(draft) {
    if (!String((draft && draft.name) || "").trim()) return "Give the checklist a name.";
    const items = (draft.items || []).filter((it) => String(it.text || "").trim() && !it.heading);
    if (!items.length) return "Add at least one item to check.";
    return "";
  }

  function open() {
    const ui = Corvus.ui;
    const CL = Corvus.checklist;
    const page = S.el("div", "checklist-dialog");

    let editing = null;
    const notice = ui.message({});

    // ------------------------------------------------------------------
    // Lists
    // ------------------------------------------------------------------

    const listsCard = S.el("div", "checklist-lists-card");
    listsCard.appendChild(notice.el);
    const listEl = S.el("div", "checklist-list");
    listsCard.appendChild(listEl);
    const addBtn = ui.button({
      variant: "secondary",
      icon: "plus",
      label: "New checklist",
      onClick: () => renderEditor({ id: CL.newId(CL.lists()), name: "", items: [{ text: "", heading: false }] }, true),
    });
    const standardBtn = ui.button({
      variant: "ghost",
      icon: "copy",
      label: "Add the standard list",
      title: "A general multicopter preflight list to start from and change",
      onClick: () => {
        const std = CL.STANDARD;
        renderEditor({
          id: CL.newId(CL.lists()),
          name: std.name,
          items: std.items.map((it) => ({ text: it.text, heading: it.heading })),
        }, true);
      },
    });
    listsCard.appendChild(ui.actions([addBtn, standardBtn]));
    page.appendChild(listsCard);

    const editorEl = S.el("div", "checklist-editor");
    editorEl.hidden = true;
    page.appendChild(editorEl);

    function paintLists() {
      if (editing) return;
      const all = CL.lists();
      const active = CL.activeList();
      ui.clear(listEl);
      if (!all.length) {
        listEl.appendChild(ui.empty(
          "No checklists. Write a new one, or start from the standard list."));
        return;
      }
      all.forEach((l) => listEl.appendChild(buildRow(l, active && active.id === l.id)));
      if (!CL.hasOwnLists()) {
        listEl.appendChild(ui.empty(
          "This is the standard list Corvus comes with. Edit it to make it yours."));
      }
      ui.refreshIcons();
    }

    function buildRow(list, isActive) {
      const row = S.el("div", "checklist-row");
      row.dataset.id = list.id;
      const text = S.el("div", "checklist-row-text");
      const nameLine = S.el("span", "checklist-row-name-line");
      nameLine.appendChild(S.el("span", "checklist-row-name", list.name));
      if (isActive) nameLine.appendChild(S.el("span", "checklist-row-badge", "On Home"));
      text.appendChild(nameLine);
      const count = list.items.filter((it) => !it.heading).length;
      const sections = list.items.filter((it) => it.heading).length;
      text.appendChild(S.el("span", "checklist-row-detail",
        `${count} ${count === 1 ? "item" : "items"}` +
        (sections ? `, ${sections} ${sections === 1 ? "section" : "sections"}` : "")));
      row.appendChild(text);
      if (!isActive) {
        row.appendChild(ui.button({
          variant: "secondary",
          size: "sm",
          label: "Use on Home",
          ariaLabel: `Show ${list.name} on the Home map`,
          onClick: () => CL.setActive(list.id).catch((err) => {
            notice.show((err && err.message) || "The choice could not be saved.", "err");
          }),
        }));
      }
      row.appendChild(ui.iconButton("pencil", {
        ariaLabel: `Edit ${list.name}`,
        title: "Edit",
        onClick: () => renderEditor(JSON.parse(JSON.stringify(list)), false),
      }));
      return row;
    }

    // ------------------------------------------------------------------
    // The editor
    // ------------------------------------------------------------------

    function renderEditor(draft, isNew) {
      editing = draft;
      listsCard.hidden = true;
      editorEl.hidden = false;
      ui.clear(editorEl);
      dialog.el.querySelector(".modal-title").textContent = isNew ? "New checklist" : "Edit checklist";

      const card = S.el("div", "checklist-editor-card");

      const nameInput = ui.input({
        value: draft.name,
        placeholder: "Survey quad",
        ariaLabel: "Checklist name",
        autocomplete: false,
        onInput: (v) => { draft.name = v; check(); },
      });
      card.appendChild(ui.field({
        label: "Name",
        control: nameInput,
        hint: "What the Home window's bar says, and how you pick it there.",
      }));

      const itemsLabel = S.el("div", "checklist-items-label", "Items");
      card.appendChild(itemsLabel);
      const itemsEl = S.el("div", "checklist-edit-items");
      card.appendChild(itemsEl);
      const itemsHint = S.el("div", "field-hint",
        "Enter adds the next item. A section groups the items under it, for " +
        "example Before power on and After power on.");
      card.appendChild(itemsHint);

      const addItemBtn = ui.button({
        variant: "secondary", size: "sm", icon: "plus", label: "Item",
        onClick: () => { draft.items.push({ text: "", heading: false }); paintItems(draft.items.length - 1); },
      });
      const addHeadingBtn = ui.button({
        variant: "ghost", size: "sm", icon: "heading", label: "Section",
        onClick: () => { draft.items.push({ text: "", heading: true }); paintItems(draft.items.length - 1); },
      });
      card.appendChild(ui.actions([addItemBtn, addHeadingBtn]));

      const msg = ui.message({});
      card.appendChild(msg.el);

      const saveBtn = ui.button({ variant: "primary", icon: "check", label: "Save", onClick: () => save() });
      const deleteBtn = isNew ? null : ui.button({
        variant: "danger", icon: "trash-2", label: "Delete",
        title: "Remove this checklist", onClick: () => remove(),
      });
      const cancelBtn = ui.button({ variant: "ghost", label: "Cancel", onClick: () => closeEditor() });
      card.appendChild(ui.actions([saveBtn, deleteBtn, cancelBtn]));
      editorEl.appendChild(card);

      function move(i, by) {
        const j = i + by;
        if (j < 0 || j >= draft.items.length) return;
        const tmp = draft.items[i];
        draft.items[i] = draft.items[j];
        draft.items[j] = tmp;
        paintItems(j);
      }

      /** Rebuild the rows and put the caret in row `focusAt`, if given. */
      function paintItems(focusAt) {
        ui.clear(itemsEl);
        const inputs = [];
        draft.items.forEach((it, i) => {
          const row = S.el("div", "checklist-edit-row" + (it.heading ? " is-heading" : ""));
          const input = ui.input({
            value: it.text,
            placeholder: it.heading ? "Section name" : "What to check",
            ariaLabel: it.heading ? `Section ${i + 1}` : `Item ${i + 1}`,
            autocomplete: false,
            onInput: (v) => { it.text = v; check(); },
          });
          input.addEventListener("keydown", (event) => {
            if (event.key === "Enter") {
              event.preventDefault();
              draft.items.splice(i + 1, 0, { text: "", heading: false });
              paintItems(i + 1);
            } else if (event.key === "Backspace" && !input.value && draft.items.length > 1) {
              event.preventDefault();
              draft.items.splice(i, 1);
              paintItems(Math.max(0, i - 1));
            }
          });
          inputs.push(input);
          row.appendChild(input);
          row.appendChild(ui.iconButton("heading", {
            size: 14,
            variant: it.heading ? "active" : undefined,
            ariaLabel: it.heading ? "Make this an item" : "Make this a section",
            title: it.heading ? "Section. Click to make it an item." : "Item. Click to make it a section.",
            onClick: () => { it.heading = !it.heading; paintItems(i); },
          }));
          const up = ui.iconButton("arrow-up", { size: 14, ariaLabel: "Move up", title: "Move up", onClick: () => move(i, -1) });
          const down = ui.iconButton("arrow-down", { size: 14, ariaLabel: "Move down", title: "Move down", onClick: () => move(i, 1) });
          up.disabled = i === 0;
          down.disabled = i === draft.items.length - 1;
          row.appendChild(up);
          row.appendChild(down);
          row.appendChild(ui.iconButton("x", {
            size: 14,
            ariaLabel: "Remove",
            title: "Remove",
            onClick: () => { draft.items.splice(i, 1); paintItems(Math.min(i, draft.items.length - 1)); },
          }));
          itemsEl.appendChild(row);
        });
        if (!draft.items.length) itemsEl.appendChild(ui.empty("No items yet."));
        ui.refreshIcons();
        check();
        if (typeof focusAt === "number" && inputs[focusAt]) inputs[focusAt].focus();
      }

      function check() {
        const problem = draftProblem(draft);
        saveBtn.disabled = !!problem;
        saveBtn.title = problem;
      }

      async function save() {
        ui.setBusy(saveBtn, true);
        msg.hide();
        try {
          await CL.saveLists(withDraft(CL.lists(), draft));
          // A list just written is the one the operator is about to fly with.
          if (isNew) await CL.setActive(draft.id);
          if (!editing) return;
          closeEditor();
          notice.show(isNew ? "Checklist added and chosen for the Home map." : "Checklist saved.", "ok");
        } catch (err) {
          ui.setBusy(saveBtn, false);
          msg.show((err && err.message) || "The checklist could not be saved.", "err");
        }
      }

      async function remove() {
        if (!window.confirm(`Remove "${draft.name || "this checklist"}"?`)) return;
        ui.setBusy(deleteBtn, true);
        try {
          await CL.saveLists(CL.lists().filter((l) => l.id !== draft.id));
          CL.resetList(draft.id);
          if (!editing) return;
          closeEditor();
          notice.show("Checklist removed.", "ok");
        } catch (err) {
          ui.setBusy(deleteBtn, false);
          msg.show((err && err.message) || "The checklist could not be removed.", "err");
        }
      }

      paintItems();
      ui.refreshIcons();
      nameInput.focus();
    }

    function closeEditor() {
      editing = null;
      ui.clear(editorEl);
      editorEl.hidden = true;
      listsCard.hidden = false;
      dialog.el.querySelector(".modal-title").textContent = TITLE;
      notice.hide();
      paintLists();
    }

    const TITLE = "Preflight checklists";
    let unsub = () => {};
    const dialog = ui.modal({
      title: TITLE,
      size: "lg",
      body: page,
      onClose: () => { editing = null; unsub(); },
    });
    unsub = CL.onChange(paintLists);
    paintLists();
    dialog.open();
    return dialog;
  }

  return { open, withDraft, draftProblem };
})();
