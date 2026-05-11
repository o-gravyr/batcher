import { getCurrentWindow } from "@tauri-apps/api/window";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { getVersion } from "@tauri-apps/api/app";
import { open, ask, message } from "@tauri-apps/plugin-dialog";
import { check } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";

const sheetZone = document.getElementById("dropzone-sheet");
const imagesZone = document.getElementById("dropzone-images");
const sheetTitle = document.getElementById("sheet-title");
const sheetHint = document.getElementById("sheet-hint");
const imagesTitle = document.getElementById("images-title");
const imagesHint = document.getElementById("images-hint");
const pickSheetBtn = document.getElementById("pick-sheet");
const pickImagesBtn = document.getElementById("pick-images");
const pickFolderBtn = document.getElementById("pick-folder");
const generateBtn = document.getElementById("generate");
const generateLabel = document.getElementById("generate-label");
const refreshBtn = document.getElementById("refresh");
const progressWrap = document.getElementById("progress-wrap");
const progressEl = document.getElementById("progress");
const progressLabel = document.getElementById("progress-label");
const progressFile = document.getElementById("progress-file");
const summary = document.getElementById("summary");
const versionEl = document.getElementById("version");
const toast = document.getElementById("toast");

const SHEET_EXTS = ["csv", "xlsx", "xlsm", "xlsb", "xls", "ods"];
const IMAGE_EXTS = ["jpg", "jpeg", "png"];

const state = {
  sheetPath: null,
  imageInputs: [], // array of paths (file or directory)
  busy: false,
};

function setBusy(b) {
  state.busy = b;
  pickSheetBtn.disabled = b;
  pickImagesBtn.disabled = b;
  pickFolderBtn.disabled = b;
  refreshBtn.disabled = b;
  refreshGenerateButton();
}

function basename(p) {
  if (!p) return "";
  const norm = p.replace(/\\/g, "/");
  const idx = norm.lastIndexOf("/");
  return idx === -1 ? norm : norm.slice(idx + 1);
}

function extOf(p) {
  const name = basename(p);
  const idx = name.lastIndexOf(".");
  return idx === -1 ? "" : name.slice(idx + 1).toLowerCase();
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

function resetUI() {
  progressWrap.hidden = true;
  progressEl.value = 0;
  progressEl.max = 1;
  progressLabel.textContent = "0 / 0";
  progressFile.textContent = "";
  summary.hidden = true;
  summary.classList.remove("error");
}

async function setSheetPath(path) {
  if (!path) return;
  if (!SHEET_EXTS.includes(extOf(path))) {
    sheetTitle.textContent = "Unsupported file";
    sheetHint.textContent = "Use CSV or XLSX";
    sheetZone.classList.remove("has-value");
    state.sheetPath = null;
    refreshGenerateButton();
    return;
  }
  state.sheetPath = path;
  sheetTitle.textContent = basename(path);
  sheetHint.textContent = "Loaded";
  sheetZone.classList.add("has-value");
  await refreshPreview();
  refreshGenerateButton();
}

async function setImageInputs(paths) {
  if (!paths || paths.length === 0) return;
  state.imageInputs = paths;
  if (paths.length === 1 && !looksLikeImage(paths[0])) {
    // Most likely a directory.
    imagesTitle.textContent = basename(paths[0]) + "/";
    imagesHint.textContent = "Folder selected";
  } else {
    const imgCount = paths.filter(looksLikeImage).length;
    imagesTitle.textContent = `${imgCount} image${imgCount === 1 ? "" : "s"}`;
    imagesHint.textContent = paths.length === 1 ? basename(paths[0]) : "Multiple files";
  }
  imagesZone.classList.add("has-value");
  await refreshPreview();
  refreshGenerateButton();
}

function looksLikeImage(p) {
  return IMAGE_EXTS.includes(extOf(p));
}

async function refreshPreview() {
  if (!state.sheetPath || state.imageInputs.length === 0) {
    return;
  }
  try {
    const result = await invoke("preview_batch", {
      spreadsheetPath: state.sheetPath,
      imageInputs: state.imageInputs,
    });
    const { rows, images, total_outputs } = result;
    sheetHint.textContent = `${rows} row${rows === 1 ? "" : "s"}`;
    imagesHint.textContent = `${images} image${images === 1 ? "" : "s"}`;
    state.previewTotal = total_outputs;
  } catch (e) {
    state.previewTotal = null;
    sheetHint.textContent = `Error: ${e}`;
  }
}

function refreshGenerateButton() {
  const ready = !!state.sheetPath && state.imageInputs.length > 0 && !state.busy;
  generateBtn.disabled = !ready;
  if (ready && state.previewTotal != null) {
    generateLabel.textContent = `Generate (${state.previewTotal} output${state.previewTotal === 1 ? "" : "s"})`;
  } else {
    generateLabel.textContent = "Generate";
  }
}

async function startProcessing() {
  if (state.busy) return;
  if (!state.sheetPath || state.imageInputs.length === 0) return;
  resetUI();
  setBusy(true);
  progressWrap.hidden = false;
  progressFile.textContent = "Preparing…";

  try {
    const result = await invoke("process_batch", {
      spreadsheetPath: state.sheetPath,
      imageInputs: state.imageInputs,
    });
    const { total, ok, errors, skipped_no_image, skipped_no_rows, output_root } = result;

    summary.hidden = false;
    if (skipped_no_rows) {
      summary.textContent = "No usable row in the spreadsheet.";
      summary.classList.add("error");
    } else if (skipped_no_image) {
      summary.textContent = "No JPG, JPEG or PNG image found.";
      summary.classList.add("error");
    } else {
      let line = `Done: ${ok} / ${total} generated.`;
      if (errors && errors.length) {
        summary.classList.add("error");
        line += ` ${errors.length} error${errors.length === 1 ? "" : "s"}.`;
      }
      line += `\nOutput: ${output_root}`;
      summary.textContent = line;
    }
  } catch (e) {
    summary.hidden = false;
    summary.classList.add("error");
    summary.textContent = `Error: ${e}`;
  } finally {
    progressFile.textContent = "";
    setBusy(false);
  }
}

// --- Disambiguating drag & drop position between the two panes ---
function paneAtPhysical(pos) {
  if (!pos) return null;
  const dpr = window.devicePixelRatio || 1;
  const cssX = pos.x / dpr;
  const cssY = pos.y / dpr;
  const lhs = sheetZone.getBoundingClientRect();
  const rhs = imagesZone.getBoundingClientRect();
  const inside = (rect) =>
    cssX >= rect.left && cssX <= rect.right && cssY >= rect.top && cssY <= rect.bottom;
  if (inside(lhs)) return "sheet";
  if (inside(rhs)) return "images";
  // Fallback: use horizontal midpoint between the two panes.
  const mid = (lhs.right + rhs.left) / 2;
  return cssX < mid ? "sheet" : "images";
}

function classifyDrop(paths) {
  if (!paths || paths.length === 0) return null;
  const first = paths[0];
  const e = extOf(first);
  if (SHEET_EXTS.includes(e)) return "sheet";
  if (IMAGE_EXTS.includes(e)) return "images";
  // No extension → likely a directory drop → images zone.
  return "images";
}

getCurrentWindow().onDragDropEvent((event) => {
  const { type } = event.payload;
  if (type === "enter" || type === "over") {
    const pane = paneAtPhysical(event.payload.position);
    sheetZone.classList.toggle("dragover", pane === "sheet");
    imagesZone.classList.toggle("dragover", pane === "images");
  } else if (type === "leave") {
    sheetZone.classList.remove("dragover");
    imagesZone.classList.remove("dragover");
  } else if (type === "drop") {
    sheetZone.classList.remove("dragover");
    imagesZone.classList.remove("dragover");
    const paths = event.payload.paths;
    const target = paneAtPhysical(event.payload.position) || classifyDrop(paths);
    if (target === "sheet") {
      setSheetPath(paths[0]);
    } else {
      setImageInputs(paths);
    }
  }
});

window.addEventListener("dragover", (e) => e.preventDefault());
window.addEventListener("drop", (e) => e.preventDefault());

// --- Pickers ---
pickSheetBtn.addEventListener("click", async () => {
  const file = await open({
    multiple: false,
    directory: false,
    filters: [{ name: "Spreadsheet", extensions: SHEET_EXTS }],
  });
  if (file) setSheetPath(typeof file === "string" ? file : file.path || file);
});

function pathOf(entry) {
  if (!entry) return null;
  if (typeof entry === "string") return entry;
  return entry.path || entry;
}

pickImagesBtn.addEventListener("click", async () => {
  const files = await open({
    multiple: true,
    directory: false,
    filters: [{ name: "Images", extensions: IMAGE_EXTS }],
  });
  if (!files) return;
  const arr = Array.isArray(files) ? files : [files];
  const paths = arr.map(pathOf).filter(Boolean);
  if (paths.length) setImageInputs(paths);
});

pickFolderBtn.addEventListener("click", async () => {
  const dir = await open({ directory: true, multiple: false });
  const p = pathOf(dir);
  if (p) setImageInputs([p]);
});

generateBtn.addEventListener("click", () => startProcessing());

// --- Progress events from Rust ---
listen("batcher://progress", ({ payload }) => {
  progressEl.max = payload.total;
  progressEl.value = payload.current;
  progressLabel.textContent = `${payload.current} / ${payload.total}`;
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
        resetUI();
        setBusy(true);
        progressWrap.hidden = false;
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
            progressLabel.textContent = `${Math.round((downloaded / contentLength) * 100)}%`;
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
      await message(`Could not check for updates: ${e}`, {
        title: "Error",
        kind: "error",
      });
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
  refreshGenerateButton();
  checkUpdate(true);
})();
