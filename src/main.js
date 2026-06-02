import { getCurrentWindow } from "@tauri-apps/api/window";
import { listen } from "@tauri-apps/api/event";
import { invoke, convertFileSrc } from "@tauri-apps/api/core";
import { getVersion } from "@tauri-apps/api/app";
import { open, ask, message } from "@tauri-apps/plugin-dialog";
import { check } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";

import logo916Url from "../Data/logo_16-9.svg";
import "@fontsource-variable/inter";

// --- Layout constants (must mirror src-tauri/src/compositor.rs) ---
// Everything is composed on a single 9:16 canvas; 1:1 is a centered crop of it.
const COMP = [1080, 1920]; // composition canvas (9:16)
const LOGO_SIZE = [368, 198]; // 9:16 logo px
const T1_TEXT_ORIGIN = [104, 350];
const T1_LOGO_POS = [357, 1297];
const MARGIN = 104;
// Top offset (% of the square viewport height) to center the 9:16 comp in a 1:1 crop.
const SQUARE_COMP_TOP = -((COMP[1] / COMP[0] - 1) / 2) * 100; // ≈ -38.889
// Safe-zone insets (top right bottom left) per aspect, as CSS inset strings.
const SAFE_INSET = {
  "9x16": "25% 6% 40% 6%",
  "1x1": "6%",
};

const DEFAULT_FONT_PX = 84;
const DEFAULT_LINE_HEIGHT = 1.18;
const CUSTOM_PARAMS = () => ({
  logo_x: "center",
  logo_y: 0.68,
  text_x: "left",
  text_y: 0.18,
  text_w: 0.81,
  font_px: DEFAULT_FONT_PX,
  line_height: DEFAULT_LINE_HEIGHT,
  letter_spacing: 0,
});
const CUSTOM_DEFAULT = () => ({ kind: "custom", tid: "custom", ...CUSTOM_PARAMS() });
const TEMPLATE1 = () => ({ kind: "template1", tid: "template1" });
const USER_TEMPLATES_KEY = "gravyr.userTemplates";

// --- Element refs ---
const $ = (id) => document.getElementById(id);
const screens = {
  folder: $("screen-folder"),
  layout: $("screen-layout"),
  progress: $("screen-progress"),
};
const folderZone = $("dropzone-folder");
const folderTitle = $("folder-title");
const folderHint = $("folder-hint");
const pickFolderBtn = $("pick-folder");

const folderLabel = $("folder-label");
const collectionSelect = $("collection-select");
const imageList = $("image-list");
const applyTemplate = $("apply-template");
const applyToSelection = $("apply-to-selection");

const previewTitle = $("preview-title");
const previewFrame = document.querySelector(".preview-frame");
const stage = $("preview-stage");
const previewComp = $("preview-comp");
const previewImg = $("preview-img");
const previewText = $("preview-text");
const previewLogo = $("preview-logo");
const safeZoneToggle = $("safe-zone");

const appHeader = document.querySelector("main > header");
const templateSelect = $("template-select");
const templateActions = $("template-actions");
const tplEdit = $("tpl-edit");
const tplSave = $("tpl-save");
const tplCancel = $("tpl-cancel");
const tplCreateRow = $("tpl-create-row");
const tplName = $("tpl-name");
const tplCreate = $("tpl-create");
const customControls = $("custom-controls");
const logoXSel = $("logo-x");
const logoYInput = $("logo-y");
const textXSel = $("text-x");
const textYInput = $("text-y");
const textWInput = $("text-w");
const fontSizeInput = $("font-size");
const lineHeightInput = $("line-height");
const letterSpacingInput = $("letter-spacing");
const previewCollection = $("preview-collection");
const previewAspect = $("preview-aspect");
const aspectLabel = $("aspect-label");
const previewLanguage = $("preview-language");
const previewMessage = $("preview-message");
const layoutWarnings = $("layout-warnings");

const backFolder = $("back-folder");
const proceedSelected = $("proceed-selected");
const proceedAll = $("proceed-all");
const fmt9x16 = $("fmt-9x16");
const fmt1x1 = $("fmt-1x1");

const progressEl = $("progress");
const progressLabel = $("progress-label");
const progressFile = $("progress-file");
const progressPct = $("progress-pct");
const progressTitle = $("progress-title");
const progressSpinner = $("progress-spinner");
const summary = $("summary");
const backLayout = $("back-layout");
const openFolderBtn = $("open-folder");

const versionEl = $("version");
const refreshBtn = $("refresh");
const toast = $("toast");

// --- State ---
const state = {
  workingFolder: null,
  busy: false,
  scanning: false,
  ws: null,
  leftCollection: null,
  templates: {}, // path -> spec
  selected: new Set(),
  active: null, // { path, rel, collection }
  anchorPath: null, // selection anchor for shift+click ranges
  orderedPaths: [], // image paths of the current collection, in render order
  preview: { collection: null, aspect: "9x16", colIdx: 0, msgIdx: 0 },
  userTemplates: loadUserTemplates(), // [{ id, name, spec:{logo_x,...} }]
  editing: false, // editing the active image's user template
};

function loadUserTemplates() {
  try {
    const raw = localStorage.getItem(USER_TEMPLATES_KEY);
    const arr = raw ? JSON.parse(raw) : [];
    return Array.isArray(arr) ? arr : [];
  } catch {
    return [];
  }
}
function saveUserTemplates() {
  try {
    localStorage.setItem(USER_TEMPLATES_KEY, JSON.stringify(state.userTemplates));
  } catch {}
}
function findUserTemplate(tid) {
  return state.userTemplates.find((t) => t.id === tid) || null;
}
function specTid(spec) {
  return (spec && (spec.tid || spec.kind)) || "template1";
}
function templateOptionsHtml() {
  let h = `<option value="template1">template-1</option><option value="custom">custom</option>`;
  for (const t of state.userTemplates) {
    h += `<option value="${esc(t.id)}">${esc(t.name)}</option>`;
  }
  return h;
}
function refreshTemplateOptions() {
  const ts = templateSelect.value;
  const as = applyTemplate.value;
  templateSelect.innerHTML = templateOptionsHtml();
  applyTemplate.innerHTML = templateOptionsHtml();
  // Restore previous selections when still valid.
  if ([...templateSelect.options].some((o) => o.value === ts)) templateSelect.value = ts;
  if ([...applyTemplate.options].some((o) => o.value === as)) applyTemplate.value = as;
  for (const { tmpl, path } of rowMap.values()) {
    tmpl.innerHTML = templateOptionsHtml();
    tmpl.value = specTid(getSpec(path));
  }
}

// Per-render index of list rows for in-place selection updates (preserves scroll).
const rowMap = new Map(); // path -> { row, cb }

function showScreen(name) {
  for (const [key, el] of Object.entries(screens)) el.hidden = key !== name;
  // The app title/subtitle only show on the folder screen.
  if (appHeader) appHeader.hidden = name !== "folder";
}

function basename(p) {
  if (!p) return "";
  const norm = p.replace(/\\/g, "/");
  const idx = norm.lastIndexOf("/");
  return idx === -1 ? norm : norm.slice(idx + 1);
}

function showToast(text) {
  toast.textContent = text;
  toast.hidden = false;
  requestAnimationFrame(() => toast.classList.add("show"));
  setTimeout(() => {
    toast.classList.remove("show");
    setTimeout(() => { toast.hidden = true; }, 200);
  }, 2200);
}

// --- Workspace loading ---
async function setWorkingFolder(path) {
  // `scanning` guards re-entry while a scan is in flight (a rapid second drop or
  // drop+picker would otherwise race two scans and clobber state).
  if (!path || state.busy || state.scanning) return;
  state.scanning = true;
  state.workingFolder = path;
  folderTitle.textContent = basename(path) + "/";
  folderHint.textContent = "Reading folder…";
  folderZone.classList.add("has-value");
  try {
    const ws = await invoke("scan_workspace", { workingFolder: path });
    initWorkspace(ws);
    showScreen("layout");
    // The frame only has a height once the layout screen is visible.
    requestAnimationFrame(sizePreview);
  } catch (e) {
    folderHint.textContent = `Error: ${e}`;
  } finally {
    state.scanning = false;
  }
}

function collectionByName(name) {
  return state.ws?.collections.find((c) => c.name === name) || null;
}

function initWorkspace(ws) {
  state.ws = ws;
  // Header label shows the selected working folder's name (instead of "Folder").
  if (state.workingFolder) folderLabel.textContent = basename(state.workingFolder);
  state.templates = {};
  state.selected = new Set();
  state.active = null;

  const firstWithImages = ws.collections.find((c) => c.images.length) || ws.collections[0];
  state.leftCollection = firstWithImages ? firstWithImages.name : null;

  // Collection selectors: left "Folder" keeps the count; image-preview shows just the name.
  collectionSelect.innerHTML = ws.collections
    .map((c) => `<option value="${esc(c.name)}">${esc(c.name)} (${c.images.length})</option>`)
    .join("");
  previewCollection.innerHTML = ws.collections
    .map((c) => `<option value="${esc(c.name)}">${esc(c.name)}</option>`)
    .join("");
  collectionSelect.value = state.leftCollection || "";
  previewCollection.value = state.leftCollection || "";

  // Language + message selectors.
  previewLanguage.innerHTML = ws.columns
    .map((c, i) => `<option value="${i}">${esc(c.language)} · ${esc(c.collection)}</option>`)
    .join("");
  previewMessage.innerHTML = ws.messages
    .map((m, i) => `<option value="${i}">${esc(m.id)}</option>`)
    .join("");

  state.preview = {
    collection: state.leftCollection,
    aspect: "9x16",
    colIdx: 0,
    msgIdx: 0,
  };
  aspectLabel.textContent = "9:16";

  // Warnings.
  if (ws.warnings && ws.warnings.length) {
    layoutWarnings.hidden = false;
    layoutWarnings.textContent = ws.warnings.join("\n");
  } else {
    layoutWarnings.hidden = true;
  }

  renderImageList();
  // Select the first image of the left collection as active.
  const coll = collectionByName(state.leftCollection);
  if (coll && coll.images.length) setActive(coll.images[0], coll.name);
  else renderPreview();
  updateProceedState();
}

function esc(s) {
  return String(s).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
}

function getSpec(path) {
  return state.templates[path] || TEMPLATE1();
}

// --- Left image list ---
function renderImageList() {
  const coll = collectionByName(state.leftCollection);
  imageList.innerHTML = "";
  rowMap.clear();
  state.orderedPaths = [];
  if (!coll || !coll.images.length) {
    imageList.innerHTML = `<p class="empty">No images in this collection.</p>`;
    return;
  }
  // Group by subfolder, preserving order.
  const groups = new Map();
  for (const img of coll.images) {
    const key = img.subfolder || "";
    if (!groups.has(key)) groups.set(key, []);
    groups.get(key).push(img);
  }
  for (const [sub, imgs] of groups) {
    if (sub) {
      const h = document.createElement("p");
      h.className = "group-head";
      h.textContent = sub;
      imageList.appendChild(h);
    }
    for (const img of imgs) {
      imageList.appendChild(imageRow(img, coll.name));
      state.orderedPaths.push(img.path);
    }
  }
}

function imageRow(img, collName) {
  const row = document.createElement("div");
  row.className = "img-row";

  // Big square hit-area (full row height) around the checkbox.
  const check = document.createElement("label");
  check.className = "row-check";
  check.addEventListener("click", (e) => e.stopPropagation());
  const cb = document.createElement("input");
  cb.type = "checkbox";
  cb.checked = state.selected.has(img.path);
  cb.addEventListener("change", (e) => {
    e.stopPropagation();
    if (cb.checked) state.selected.add(img.path);
    else state.selected.delete(img.path);
    state.anchorPath = img.path;
    updateSelectionUI();
    updateProceedState();
  });
  check.appendChild(cb);

  const thumb = document.createElement("img");
  thumb.className = "thumb";
  thumb.loading = "lazy";
  thumb.src = convertFileSrc(img.path);

  const name = document.createElement("span");
  name.className = "img-name";
  name.textContent = basename(img.path);

  const tmpl = document.createElement("select");
  tmpl.className = "select select-sm row-tmpl";
  tmpl.innerHTML = templateOptionsHtml();
  tmpl.value = specTid(getSpec(img.path));
  tmpl.addEventListener("click", (e) => e.stopPropagation());
  tmpl.addEventListener("change", (e) => {
    e.stopPropagation();
    setSpecKind(img.path, tmpl.value);
    if (state.active && state.active.path === img.path) {
      state.editing = false;
      syncRightPanel();
    }
    renderPreview();
  });

  row.append(check, thumb, name, tmpl);
  row.addEventListener("click", (e) => onRowClick(img, collName, e));
  rowMap.set(img.path, { row, cb, tmpl, path: img.path });
  return row;
}

// File-explorer style selection: plain click = single; Cmd/Ctrl+click = toggle;
// Shift+click = range from anchor. The clicked row also becomes the active preview.
function onRowClick(img, collName, e) {
  const path = img.path;
  if (e.shiftKey && state.anchorPath) {
    const a = state.orderedPaths.indexOf(state.anchorPath);
    const b = state.orderedPaths.indexOf(path);
    if (a !== -1 && b !== -1) {
      const [lo, hi] = a < b ? [a, b] : [b, a];
      state.selected = new Set(state.orderedPaths.slice(lo, hi + 1));
    } else {
      state.selected = new Set([path]);
    }
  } else if (e.metaKey || e.ctrlKey) {
    if (state.selected.has(path)) state.selected.delete(path);
    else state.selected.add(path);
    state.anchorPath = path;
  } else {
    state.selected = new Set([path]);
    state.anchorPath = path;
  }
  setActive(img, collName); // re-renders list + preview; reflects selection
  updateProceedState();
}

function selectAllVisible() {
  state.selected = new Set(state.orderedPaths);
  updateSelectionUI();
  updateProceedState();
}

// Reflect selection/active state on existing rows without rebuilding the list.
function updateSelectionUI() {
  for (const [path, { row, cb }] of rowMap) {
    const sel = state.selected.has(path);
    row.classList.toggle("selected", sel);
    cb.checked = sel;
    row.classList.toggle("active", !!state.active && state.active.path === path);
  }
}

// `tid` is "template1", "custom", or a user-template id.
function setSpecKind(path, tid) {
  if (tid === "template1") {
    state.templates[path] = TEMPLATE1();
  } else if (tid === "custom") {
    if (specTid(state.templates[path]) !== "custom") state.templates[path] = CUSTOM_DEFAULT();
  } else {
    const t = findUserTemplate(tid);
    state.templates[path] = t
      ? { kind: "custom", tid, ...t.spec }
      : CUSTOM_DEFAULT();
  }
}

function setActive(img, collName) {
  state.active = { path: img.path, rel: img.rel, subfolder: img.subfolder, stem: img.stem, collection: collName };
  state.editing = false;
  state.preview.collection = collName;
  previewCollection.value = collName;
  // Default the preview language to a column matching this collection.
  const idx = state.ws.columns.findIndex((c) => c.collection === collName);
  if (idx >= 0) {
    state.preview.colIdx = idx;
    previewLanguage.value = String(idx);
  }
  updateSelectionUI();
  syncRightPanel();
  renderPreview();
}

// --- Right panel ---
const customFields = () => [logoXSel, logoYInput, textXSel, textYInput, textWInput, fontSizeInput, lineHeightInput, letterSpacingInput];

function syncRightPanel() {
  if (!state.active) return;
  const spec = getSpec(state.active.path);
  const tid = specTid(spec);
  templateSelect.value = tid;

  const isUser = !!findUserTemplate(tid);
  // Editable when freeform custom, or when editing a user template.
  const editable = tid === "custom" || (isUser && state.editing);

  // template-1 has no params → show its equivalent (≈ CUSTOM_DEFAULT) values.
  const display = tid === "template1" ? CUSTOM_DEFAULT() : spec;
  logoXSel.value = display.logo_x;
  logoYInput.value = Math.round(display.logo_y * 100);
  textXSel.value = display.text_x;
  textYInput.value = Math.round(display.text_y * 100);
  textWInput.value = Math.round(display.text_w * 100);
  fontSizeInput.value = Math.round(display.font_px ?? DEFAULT_FONT_PX);
  lineHeightInput.value = display.line_height ?? DEFAULT_LINE_HEIGHT;
  letterSpacingInput.value = display.letter_spacing ?? 0;

  customControls.classList.toggle("is-locked", !editable);
  for (const el of customFields()) el.disabled = !editable;

  // Template action buttons.
  templateActions.hidden = !isUser;
  tplEdit.hidden = !isUser || state.editing;
  tplSave.hidden = !(isUser && state.editing);
  tplCancel.hidden = !(isUser && state.editing);
  tplCreateRow.hidden = tid !== "custom";
}

function readCustomParams() {
  const fontPx = Number(fontSizeInput.value);
  const lineH = Number(lineHeightInput.value);
  return {
    logo_x: logoXSel.value,
    logo_y: clamp01(Number(logoYInput.value) / 100),
    text_x: textXSel.value,
    text_y: clamp01(Number(textYInput.value) / 100),
    text_w: clamp01(Number(textWInput.value) / 100),
    font_px: fontPx > 0 ? fontPx : DEFAULT_FONT_PX,
    line_height: lineH > 0 ? lineH : DEFAULT_LINE_HEIGHT,
    letter_spacing: Number(letterSpacingInput.value) || 0,
  };
}
function clamp01(n) {
  if (Number.isNaN(n)) return 0;
  return Math.min(1, Math.max(0, n));
}

function onTemplateSelectChange() {
  if (!state.active) return;
  state.editing = false;
  setSpecKind(state.active.path, templateSelect.value);
  const entry = rowMap.get(state.active.path);
  if (entry) entry.tmpl.value = specTid(getSpec(state.active.path));
  syncRightPanel();
  renderPreview();
}

// Live edit: only writes when the fields are editable (custom or editing a template).
function onCustomInputChange() {
  if (!state.active) return;
  const tid = specTid(getSpec(state.active.path));
  const editable = tid === "custom" || (findUserTemplate(tid) && state.editing);
  if (!editable) return;
  state.templates[state.active.path] = { kind: "custom", tid, ...readCustomParams() };
  renderPreview();
}

// --- User templates: create / edit / save / cancel ---
function onCreateTemplate() {
  if (!state.active) return;
  const params = readCustomParams();
  const name = tplName.value.trim() || `Template ${state.userTemplates.length + 1}`;
  const id = "t" + Date.now().toString(36);
  state.userTemplates.push({ id, name, spec: params });
  saveUserTemplates();
  state.templates[state.active.path] = { kind: "custom", tid: id, ...params };
  tplName.value = "";
  refreshTemplateOptions();
  templateSelect.value = id;
  syncRightPanel();
  renderPreview();
}

function onEditTemplate() {
  state.editing = true;
  syncRightPanel();
}

function onSaveTemplate() {
  if (!state.active) return;
  const tid = specTid(getSpec(state.active.path));
  const t = findUserTemplate(tid);
  if (!t) return;
  t.spec = readCustomParams();
  saveUserTemplates();
  // Re-apply to every image using this template so snapshots stay in sync.
  for (const [p, sp] of Object.entries(state.templates)) {
    if (specTid(sp) === tid) state.templates[p] = { kind: "custom", tid, ...t.spec };
  }
  state.editing = false;
  syncRightPanel();
  renderPreview();
}

function onCancelTemplate() {
  if (!state.active) return;
  state.editing = false;
  const tid = specTid(getSpec(state.active.path));
  const t = findUserTemplate(tid);
  if (t) state.templates[state.active.path] = { kind: "custom", tid, ...t.spec };
  syncRightPanel();
  renderPreview();
}

// --- Preview rendering (CSS approximation) ---
function previewImagePath() {
  if (!state.active) return null;
  const coll = collectionByName(state.preview.collection);
  if (!coll) return state.active.path;
  // Same scene across collections, ignoring extension: `base` stores .jpg while
  // the other collections store .png, so full `rel` won't match across them.
  const same = coll.images.find(
    (i) => i.subfolder === state.active.subfolder && i.stem === state.active.stem
  );
  if (same) return same.path;
  return coll.images.length ? coll.images[0].path : state.active.path;
}

// Element positions, always computed in the 9:16 composition space (the 1:1 view
// is just a centered crop of that same composition).
function compLayout(spec) {
  const [cw, ch] = COMP;
  const [logoW, logoH] = LOGO_SIZE;
  const logoWpct = (logoW / cw) * 100;
  let textXpct, textYpct, textWpct, logoXpct, logoYpct, fontPx, lineHeight, letterEm;

  if (spec.kind === "custom") {
    textWpct = clamp01(spec.text_w) * 100;
    const textWpx = clamp01(spec.text_w) * cw;
    textXpct = (anchorPx(spec.text_x, cw, textWpx) / cw) * 100;
    textYpct = clamp01(spec.text_y) * 100;
    logoXpct = (anchorPx(spec.logo_x, cw, logoW) / cw) * 100;
    // logo_y is the logo's CENTER → convert to a top% for CSS positioning.
    logoYpct = (clamp01(spec.logo_y) - logoH / ch / 2) * 100;
    fontPx = spec.font_px ?? DEFAULT_FONT_PX;
    lineHeight = spec.line_height ?? DEFAULT_LINE_HEIGHT;
    letterEm = (spec.letter_spacing ?? 0) / 100;
  } else {
    textXpct = (T1_TEXT_ORIGIN[0] / cw) * 100;
    textYpct = (T1_TEXT_ORIGIN[1] / ch) * 100;
    textWpct = ((cw - 2 * T1_TEXT_ORIGIN[0]) / cw) * 100;
    logoXpct = (T1_LOGO_POS[0] / cw) * 100;
    logoYpct = (T1_LOGO_POS[1] / ch) * 100;
    fontPx = DEFAULT_FONT_PX;
    lineHeight = DEFAULT_LINE_HEIGHT;
    letterEm = 0;
  }
  // Font size as cqw (1% of the 1080-wide comp width).
  const fontCqw = (fontPx / cw) * 100;
  return { textXpct, textYpct, textWpct, logoXpct, logoYpct, logoWpct, fontCqw, lineHeight, letterEm };
}

function anchorPx(anchor, canvasW, contentW) {
  if (anchor === "left") return MARGIN;
  if (anchor === "right") return canvasW - contentW - MARGIN;
  return (canvasW - contentW) / 2; // center
}

// Width = (9:16 at full available height) and stays constant across aspects, so
// switching to 1:1 only shrinks the height. Driven from the frame's height so the
// `auto` grid column can size to an explicit width (no flex-transfer collapse).
function sizePreview() {
  if (!previewFrame) return;
  const avail = previewFrame.clientHeight;
  if (!avail) return;
  const h = Math.min(avail, window.innerHeight * 0.9); // cap at 90vh
  const w = (h * 9) / 16;
  // Published on :root so both the stage and the center grid column read it.
  document.documentElement.style.setProperty("--stage-w", `${Math.round(w)}px`);
}

function renderPreview() {
  if (!state.ws) return;
  sizePreview();
  const aspect = state.preview.aspect;
  const path = previewImagePath();
  if (path) previewImg.src = convertFileSrc(path);
  else previewImg.removeAttribute("src");

  const spec = state.active ? getSpec(state.active.path) : TEMPLATE1();
  const L = compLayout(spec);

  // Element positions live on the 9:16 composition.
  previewComp.style.setProperty("--text-x", `${L.textXpct}%`);
  previewComp.style.setProperty("--text-y", `${L.textYpct}%`);
  previewComp.style.setProperty("--text-w", `${L.textWpct}%`);
  previewComp.style.setProperty("--logo-x", `${L.logoXpct}%`);
  previewComp.style.setProperty("--logo-y", `${L.logoYpct}%`);
  previewComp.style.setProperty("--logo-w", `${L.logoWpct}%`);
  previewComp.style.setProperty("--font-size", `${L.fontCqw}cqw`);
  previewComp.style.setProperty("--line-height", `${L.lineHeight}`);
  previewComp.style.setProperty("--letter-spacing", `${L.letterEm}em`);

  // The viewport aspect ratio + comp offset realize the centered 1:1 crop.
  if (aspect === "1x1") {
    stage.style.setProperty("--view-ar", "1 / 1");
    previewComp.style.setProperty("--comp-top", `${SQUARE_COMP_TOP}%`);
  } else {
    stage.style.setProperty("--view-ar", "9 / 16");
    previewComp.style.setProperty("--comp-top", "0%");
  }
  stage.style.setProperty("--safe-inset", SAFE_INSET[aspect] || SAFE_INSET["9x16"]);

  previewLogo.src = logo916Url;

  // Text from chosen language column + message row.
  const col = state.ws.columns[state.preview.colIdx];
  const msg = state.ws.messages[state.preview.msgIdx];
  const text = msg && col ? (msg.cells[state.preview.colIdx] || "") : "";
  previewText.textContent = text || "—";
  previewText.classList.toggle("empty-text", !text);

  // Caption.
  const label = state.active ? basename(state.active.path) : "—";
  previewTitle.textContent = label;
}

// --- Selection / proceed ---
function selectedFormats() {
  return [fmt9x16.checked && "9x16", fmt1x1.checked && "1x1"].filter(Boolean);
}

function updateProceedState() {
  const noFormat = selectedFormats().length === 0;
  proceedAll.disabled = state.busy || noFormat;
  proceedSelected.disabled = state.busy || noFormat || state.selected.size === 0;
}

function buildPlan(onlySelected) {
  return {
    default_template: TEMPLATE1(),
    templates_by_image: state.templates,
    only_selected: onlySelected,
    selected: Array.from(state.selected),
    formats: selectedFormats(),
  };
}

async function proceed(onlySelected) {
  if (state.busy || !state.workingFolder) return;
  resetProgress();
  showScreen("progress");
  setBusy(true);
  progressTitle.textContent = "Generating…";
  progressSpinner.hidden = false;
  progressFile.textContent = "Preparing…";
  try {
    const result = await invoke("process_batch", {
      workingFolder: state.workingFolder,
      plan: buildPlan(onlySelected),
    });
    const { total, ok, errors, skipped_no_image, skipped_no_rows, output_root } = result;
    state.lastOutput = output_root;
    summary.hidden = false;
    if (skipped_no_rows) {
      progressTitle.textContent = "Nothing to generate";
      summary.textContent = "No usable message in the Batcher sheet.";
      summary.classList.add("error");
    } else if (skipped_no_image) {
      progressTitle.textContent = "Nothing to generate";
      summary.textContent = "No image found in the referenced collections.";
      summary.classList.add("error");
    } else {
      const hasErrors = errors && errors.length;
      progressTitle.textContent = hasErrors ? "Completed with errors" : "Done";
      let line = `${ok} / ${total} images generated.`;
      if (hasErrors) {
        summary.classList.add("error");
        line += ` ${errors.length} error${errors.length === 1 ? "" : "s"}.`;
      }
      line += `\nOutput: ${output_root}`;
      summary.textContent = line;
    }
  } catch (e) {
    progressTitle.textContent = "Failed";
    summary.hidden = false;
    summary.classList.add("error");
    summary.textContent = `Error: ${e}`;
  } finally {
    progressSpinner.hidden = true;
    progressFile.textContent = "";
    setBusy(false);
    backLayout.hidden = false;
    if (state.lastOutput) openFolderBtn.hidden = false;
  }
}

function setBusy(b) {
  state.busy = b;
  pickFolderBtn.disabled = b;
  refreshBtn.disabled = b;
  applyToSelection.disabled = b;
  updateProceedState(); // handles proceedAll/proceedSelected (busy + formats + selection)
}

function resetProgress() {
  progressEl.value = 0;
  progressEl.max = 1;
  progressLabel.textContent = "0 / 0";
  progressPct.textContent = "0%";
  progressFile.textContent = "";
  progressSpinner.hidden = false;
  summary.hidden = true;
  summary.classList.remove("error");
  summary.textContent = "";
  backLayout.hidden = true;
  openFolderBtn.hidden = true;
  state.lastOutput = null;
}

// --- Event wiring ---
collectionSelect.addEventListener("change", () => {
  state.leftCollection = collectionSelect.value;
  // Selection is per-collection: dropping it avoids generating images from a
  // collection the user can no longer see (their checkboxes are now hidden).
  state.selected = new Set();
  state.anchorPath = null;
  renderImageList();
  const coll = collectionByName(state.leftCollection);
  if (coll && coll.images.length) setActive(coll.images[0], coll.name);
  updateProceedState();
});

applyToSelection.addEventListener("click", () => {
  const kind = applyTemplate.value;
  for (const path of state.selected) setSpecKind(path, kind);
  renderImageList();
  updateSelectionUI();
  if (state.active) syncRightPanel();
  renderPreview();
});

// Cmd/Ctrl+A selects every image in the current collection (layout screen only,
// and not while typing in a field).
document.addEventListener("keydown", (e) => {
  if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "a") {
    if (screens.layout.hidden) return;
    const tag = (document.activeElement?.tagName || "").toLowerCase();
    if (tag === "input" || tag === "select" || tag === "textarea") return;
    if (!state.orderedPaths.length) return;
    e.preventDefault();
    selectAllVisible();
  }
});

templateSelect.addEventListener("change", onTemplateSelectChange);
tplCreate.addEventListener("click", onCreateTemplate);
tplEdit.addEventListener("click", onEditTemplate);
tplSave.addEventListener("click", onSaveTemplate);
tplCancel.addEventListener("click", onCancelTemplate);
for (const el of [logoXSel, logoYInput, textXSel, textYInput, textWInput, fontSizeInput, lineHeightInput, letterSpacingInput]) {
  el.addEventListener("input", onCustomInputChange);
  el.addEventListener("change", onCustomInputChange);
}

previewCollection.addEventListener("change", () => {
  state.preview.collection = previewCollection.value;
  renderPreview();
});
// Aspect is a toggle button: click loops 9:16 ↔ 1:1 (no dropdown).
previewAspect.addEventListener("click", () => {
  state.preview.aspect = state.preview.aspect === "9x16" ? "1x1" : "9x16";
  aspectLabel.textContent = state.preview.aspect === "9x16" ? "9:16" : "1:1";
  renderPreview();
});
previewLanguage.addEventListener("change", () => {
  state.preview.colIdx = Number(previewLanguage.value);
  renderPreview();
});
previewMessage.addEventListener("change", () => {
  state.preview.msgIdx = Number(previewMessage.value);
  renderPreview();
});
safeZoneToggle.addEventListener("change", () => {
  stage.classList.toggle("show-safe", safeZoneToggle.checked);
});

// Keep the preview sized to the available height as the window resizes.
if (previewFrame && "ResizeObserver" in window) {
  new ResizeObserver(() => sizePreview()).observe(previewFrame);
}
window.addEventListener("resize", sizePreview);

fmt9x16.addEventListener("change", updateProceedState);
fmt1x1.addEventListener("change", updateProceedState);

backFolder.addEventListener("click", () => showScreen("folder"));
backLayout.addEventListener("click", () => showScreen("layout"));
openFolderBtn.addEventListener("click", () => {
  if (state.lastOutput) invoke("open_path", { path: state.lastOutput });
});
proceedAll.addEventListener("click", () => proceed(false));
proceedSelected.addEventListener("click", () => proceed(true));

// --- Drag & drop: the whole window is one drop target for the working folder. ---
getCurrentWindow().onDragDropEvent((event) => {
  if (state.busy) return;
  const { type } = event.payload;
  if (type === "enter" || type === "over") {
    folderZone.classList.add("dragover");
  } else if (type === "leave") {
    folderZone.classList.remove("dragover");
  } else if (type === "drop") {
    folderZone.classList.remove("dragover");
    const paths = event.payload.paths || [];
    if (paths.length === 0) return;
    setWorkingFolder(paths[0]);
  }
});

window.addEventListener("dragover", (e) => e.preventDefault());
window.addEventListener("drop", (e) => e.preventDefault());

function pathOf(entry) {
  if (!entry) return null;
  if (typeof entry === "string") return entry;
  return entry.path || entry;
}

pickFolderBtn.addEventListener("click", async () => {
  const dir = await open({ directory: true, multiple: false });
  const p = pathOf(dir);
  if (p) setWorkingFolder(p);
});

// --- Progress events from Rust ---
listen("batcher://progress", ({ payload }) => {
  progressEl.max = payload.total;
  progressEl.value = payload.current;
  progressLabel.textContent = `${payload.current} / ${payload.total}`;
  const pct = payload.total ? Math.round((payload.current / payload.total) * 100) : 0;
  progressPct.textContent = `${pct}%`;
  progressFile.textContent = payload.file;
});

// --- Updates ---
async function checkUpdate(silent) {
  try {
    const update = await check();
    if (update) {
      const yes = await ask(
        `A new version (${update.version}) is available.\n\nInstall it now? The app will restart.`,
        { title: "Update available", okLabel: "Install", cancelLabel: "Later" }
      );
      if (yes) {
        showScreen("progress");
        resetProgress();
        setBusy(true);
        progressTitle.textContent = "Downloading update…";
        progressLabel.textContent = "Downloading…";
        let downloaded = 0;
        let contentLength = 1;
        await update.downloadAndInstall((event) => {
          if (event.event === "Started") {
            contentLength = event.data.contentLength || 1;
            progressEl.max = contentLength;
            progressEl.value = 0;
          } else if (event.event === "Progress") {
            downloaded += event.data.chunkLength;
            progressEl.value = downloaded;
            const p = Math.round((downloaded / contentLength) * 100);
            progressPct.textContent = `${p}%`;
            progressLabel.textContent = `${p}%`;
          } else if (event.event === "Finished") {
            progressLabel.textContent = "Installing…";
          }
        });
        await relaunch();
      }
    } else if (!silent) {
      showToast("App is up to date");
    }
  } catch (e) {
    if (!silent) {
      await message(`Could not check for updates: ${e}`, { title: "Error", kind: "error" });
    } else {
      console.warn("update check failed:", e);
    }
  }
}

refreshBtn.addEventListener("click", () => checkUpdate(false));

// --- Init ---
(async () => {
  try {
    versionEl.textContent = `v${await getVersion()}`;
  } catch {}
  refreshTemplateOptions(); // populate template + apply selects (incl. user templates)
  showScreen("folder");
  checkUpdate(true);
})();
