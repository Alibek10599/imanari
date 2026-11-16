"use strict";
(() => {
  const main = document.getElementById("app");
  const bar = document.getElementById("dev-bar");
  const pendingKeys = new Map();
  let config = null;
  let session = null;
  let uid = 0;

  function el(tag, attrs, ...children) {
    const node = document.createElement(tag);
    for (const [k, v] of Object.entries(attrs || {})) {
      if (v === null || v === undefined || v === false) continue;
      if (k === "text") node.textContent = String(v);
      else if (k.startsWith("on")) node.addEventListener(k.slice(2), v);
      else node.setAttribute(k, v === true ? "" : String(v));
    }
    for (const c of children) if (c !== null && c !== undefined) node.append(c);
    return node;
  }

  function nextId(prefix) {
    uid += 1;
    return prefix + "-" + uid;
  }

  function keyFor(action, input) {
    const id = action + "\u0000" + JSON.stringify(input);
    if (!pendingKeys.has(id)) pendingKeys.set(id, crypto.randomUUID());
    return { id, key: pendingKeys.get(id) };
  }

  function settle(k, r) {
    const retryable = !r.ok && (r.network || r.status === 503);
    if (!retryable) pendingKeys.delete(k.id);
  }

  async function call(action, input, key) {
    const headers = { "Content-Type": "application/json", "X-Nari-CSRF": session.user.csrf };
    if (key) headers["Idempotency-Key"] = key;
    let resp;
    try {
      resp = await fetch("/api/actions/" + encodeURIComponent(action), {
        method: "POST",
        credentials: "same-origin",
        headers,
        body: JSON.stringify(input),
      });
    } catch (e) {
      return { ok: false, network: true, error: { code: "NETWORK", message: "Network error. The change may or may not have been saved; retrying is safe." } };
    }
    let body = null;
    try {
      body = await resp.json();
    } catch (e) {
      body = null;
    }
    if (resp.ok && body && Object.prototype.hasOwnProperty.call(body, "data")) return { ok: true, data: body.data };
    const error = (body && body.error) || { code: "HTTP_" + resp.status, message: "Request failed (" + resp.status + ")." };
    return { ok: false, status: resp.status, error };
  }

  async function loadSession() {
    const resp = await fetch("/_nari/session", { credentials: "same-origin" });
    session = resp.ok ? (await resp.json()).data : null;
  }

  function renderBar() {
    bar.replaceChildren();
    if (config.identity === "oidc") {
      bar.setAttribute("aria-label", "Account");
      if (!session || !session.user) return;
      const out = el("button", { type: "button", text: "Sign out" });
      out.addEventListener("click", async () => {
        out.disabled = true;
        await fetch("/_nari/logout", {
          method: "POST",
          credentials: "same-origin",
          headers: { "Content-Type": "application/json", "X-Nari-CSRF": session.user.csrf },
          body: "{}",
        });
        await loadSession();
        render();
      });
      bar.append(el("span", { text: "Signed in as " + session.user.label }), out);
      return;
    }
    if (!session || !session.dev) return;
    const id = nextId("dev-user");
    const select = el("select", { id });
    for (const u of session.dev.users) {
      select.append(el("option", { value: u.key, selected: session.user && session.user.id === u.id, text: u.label }));
    }
    select.addEventListener("change", async () => {
      select.disabled = true;
      await fetch("/_nari/dev/select", {
        method: "POST",
        credentials: "same-origin",
        headers: { "Content-Type": "application/json", "X-Nari-CSRF": session.dev.csrf },
        body: JSON.stringify({ user: select.value }),
      });
      await loadSession();
      render();
    });
    bar.append(el("span", { class: "dev-note", text: "Local development identity" }), el("label", { for: id, text: "Signed in as" }), select);
  }

  function formatValue(type, value) {
    if (type.name === "Bool") return value ? "Yes" : "No";
    if (value === null || value === undefined) return "";
    return String(value);
  }

  function renderScreen(screen) {
    document.title = screen.title;
    const state = { items: [], next: null, loading: true, error: null, more: false };
    const alertBox = el("div", { role: "alert", class: "alert", tabindex: "-1" });
    const status = el("p", { role: "status", class: "status" });
    const announce = (text) => {
      status.textContent = text;
    };
    const showError = (text) => {
      alertBox.replaceChildren(el("p", { text }));
      alertBox.focus();
    };
    const clearError = () => alertBox.replaceChildren();
    const listViews = [];

    async function refresh(after) {
      state.loading = true;
      state.error = null;
      drawLists();
      const input = after ? { after } : {};
      const r = await call(screen.query, input, null);
      state.loading = false;
      if (r.ok) {
        state.items = after ? state.items.concat(r.data.items) : r.data.items;
        state.next = r.data.next_cursor;
      } else {
        state.error = r.error.message;
      }
      drawLists();
    }

    function drawLists() {
      for (const view of listViews) view();
    }

    const root = el("section", { class: "screen" }, el("h1", { text: screen.title }), alertBox, status);

    for (const form of screen.forms) {
      const controls = {};
      const fieldErrors = {};
      const node = el("form", { novalidate: false, "aria-label": form.name.replace(/_/g, " ") });
      for (const input of form.inputs) {
        const id = nextId("input-" + input.name);
        const errId = id + "-error";
        let control;
        if (input.type.name === "Bool") {
          control = el("input", { id, name: input.name, type: "checkbox" });
        } else if (input.type.name === "Int") {
          control = el("input", { id, name: input.name, type: "number", step: "1", required: true, placeholder: input.placeholder });
        } else {
          const text = input.type.name === "Text";
          control = el("input", {
            id,
            name: input.name,
            type: "text",
            required: text ? input.type.min_length > 0 : true,
            minlength: text ? input.type.min_length : null,
            maxlength: text ? input.type.max_length : null,
            pattern: input.type.name === "UUID" ? "[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}" : null,
            placeholder: input.placeholder,
            autocomplete: "off",
          });
        }
        controls[input.name] = control;
        fieldErrors[input.name] = el("span", { id: errId, class: "field-error" });
        control.setAttribute("aria-describedby", errId);
        node.append(el("div", { class: "field" }, el("label", { for: id, text: input.label }), control, fieldErrors[input.name]));
      }
      const submit = el("button", { type: "submit", text: "Save" });
      node.append(submit);
      let pending = false;
      node.addEventListener("submit", async (ev) => {
        ev.preventDefault();
        if (pending) return;
        if (!node.reportValidity()) return;
        const input = {};
        for (const spec of form.inputs) {
          const c = controls[spec.name];
          c.removeAttribute("aria-invalid");
          fieldErrors[spec.name].textContent = "";
          if (spec.type.name === "Bool") input[spec.name] = c.checked;
          else if (spec.type.name === "Int") input[spec.name] = Number.parseInt(c.value, 10);
          else input[spec.name] = c.value;
        }
        pending = true;
        submit.disabled = true;
        node.setAttribute("aria-busy", "true");
        clearError();
        announce("Saving…");
        const k = keyFor(form.submit, input);
        const r = await call(form.submit, input, k.key);
        settle(k, r);
        pending = false;
        submit.disabled = false;
        node.removeAttribute("aria-busy");
        if (r.ok) {
          node.reset();
          announce("Saved.");
          refresh();
          return;
        }
        announce("");
        showError(r.error.message);
        const field = r.error.field && controls[r.error.field];
        if (field) {
          field.setAttribute("aria-invalid", "true");
          fieldErrors[r.error.field].textContent = r.error.message;
          field.focus();
        }
      });
      root.append(node);
    }

    for (const list of screen.lists) {
      const headingId = nextId("list");
      const container = el("section", { class: "list", "aria-labelledby": headingId });
      const body = el("div", { "aria-live": "polite" });
      container.append(el("h2", { id: headingId, text: list.name.replace(/_/g, " ") }), body);
      const draw = () => {
        body.replaceChildren();
        if (state.loading && state.items.length === 0) {
          body.append(el("p", { class: "loading", text: "Loading…" }));
          return;
        }
        if (state.error) {
          body.append(el("p", { class: "error", text: "Could not load: " + state.error }), el("button", { type: "button", text: "Retry", onclick: () => refresh() }));
          return;
        }
        if (state.items.length === 0) {
          body.append(el("p", { class: "empty", text: "Nothing here yet." }));
          return;
        }
        const ul = el("ul");
        for (const item of state.items) {
          const li = el("li");
          const dl = el("dl");
          for (const f of list.fields) {
            dl.append(el("dt", { text: f.name.replace(/_/g, " ") }), el("dd", { text: formatValue(f.type, item[f.name]) }));
          }
          li.append(dl);
          for (const b of list.buttons) {
            const btn = el("button", { type: "button", text: b.label });
            btn.addEventListener("click", async () => {
              if (btn.disabled) return;
              const input = {};
              for (const a of b.arguments) input[a.input] = a.from === "item" ? item[a.field] : a.value;
              btn.disabled = true;
              btn.setAttribute("aria-busy", "true");
              clearError();
              const k = keyFor(b.action, input);
              const r = await call(b.action, input, k.key);
              settle(k, r);
              btn.disabled = false;
              btn.removeAttribute("aria-busy");
              if (r.ok) {
                announce(b.label + ": done.");
                refresh();
              } else if (r.status === 409 && r.error.code === "REVISION_CONFLICT") {
                showError("This item changed since it was loaded. The list has been refreshed; review it and try again.");
                refresh();
              } else {
                showError(r.error.message);
              }
            });
            li.append(btn);
          }
          ul.append(li);
        }
        body.append(ul);
        if (state.next) {
          const more = el("button", { type: "button", text: "Load more" });
          more.addEventListener("click", () => {
            more.disabled = true;
            refresh(state.next);
          });
          body.append(more);
        }
      };
      listViews.push(draw);
      root.append(container);
    }
    main.replaceChildren(root);
    refresh();
  }

  function render() {
    renderBar();
    const screen = config.screens.find((s) => s.route === location.pathname);
    if (!screen) {
      document.title = "Not found";
      main.replaceChildren(el("h1", { text: "Not found" }));
      return;
    }
    if (!session || !session.user) {
      const prompt = config.identity === "oidc"
        ? el("p", {}, el("a", { href: "/_nari/login", text: "Sign in" }), " to continue.")
        : el("p", { role: "alert", text: "Not signed in. Run `nari dev` and open the bootstrap URL it prints." });
      main.replaceChildren(el("h1", { text: screen.title }), prompt);
      return;
    }
    renderScreen(screen);
  }

  async function start() {
    const m = /^#bootstrap=([0-9a-f]{64})$/.exec(location.hash);
    if (m) {
      history.replaceState(null, "", location.pathname);
      await fetch("/_nari/bootstrap", {
        method: "POST",
        credentials: "same-origin",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ token: m[1] }),
      });
    }
    config = await (await fetch("/assets/nari.json", { credentials: "same-origin" })).json();
    await loadSession();
    render();
  }

  start().catch(() => {
    main.replaceChildren(el("p", { role: "alert", text: "The application failed to start. Reload to try again." }));
  });
})();
