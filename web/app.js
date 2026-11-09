const log = document.querySelector("#log");
const form = document.querySelector("#composer");
const input = document.querySelector("#input");
const send = document.querySelector("#send");
const newChat = document.querySelector("#new-chat");
const attach = document.querySelector("#attach");
const fileInput = document.querySelector("#file");
const fileChip = document.querySelector("#file-chip");
const mic = document.querySelector("#mic");
const chats = document.querySelector("#chats");
const modelLabel = document.querySelector("#model");
const conn = document.querySelector("#conn");

const suggestions = [
  "Calcule 2 + 2 em Python",
  "Liste os arquivos de /tmp",
  "Explique o que esta sandbox consegue fazer",
];

let socket;
let busy = false;
let items = [];
let sessions = [];
let currentId = "";
let attachment = null;
let recognition = null;
let listening = false;

function escapeHtml(value) {
  return value
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;");
}

function renderMarkdown(text) {
  const parts = escapeHtml(text).split(/```/);
  return parts
    .map((part, index) => {
      if (index % 2 === 1) {
        const code = part.replace(/^\w+\n/, "");
        return `<pre><code>${code}</code></pre>`;
      }
      const html = part
        .replace(/`([^`]+)`/g, "<code>$1</code>")
        .replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>")
        .replace(/\n/g, "<br>");
      return html ? `<p>${html}</p>` : "";
    })
    .join("");
}

function setBusy(value) {
  busy = value;
  send.disabled = value;
  newChat.disabled = value;
  attach.disabled = value;
  mic.disabled = value;
}

function setConn(text, on) {
  conn.textContent = text;
  conn.classList.toggle("on", on);
}

function renderChats() {
  chats.innerHTML = sessions
    .map(
      (session) => `
        <div class="chat-row ${session.id === currentId ? "active" : ""}">
          <button type="button" data-open="${session.id}">${escapeHtml(session.title)}</button>
          <button class="delete" type="button" data-delete="${session.id}" aria-label="Apagar conversa">×</button>
        </div>`,
    )
    .join("");
}

function renderThread() {
  if (items.length === 0) {
    log.innerHTML = `
      <div class="empty">
        <h1>Como posso ajudar?</h1>
        <div class="suggestions">
          ${suggestions
            .map(
              (text, index) =>
                `<button type="button" data-suggestion="${index}">${escapeHtml(text)}</button>`,
            )
            .join("")}
        </div>
      </div>`;
    return;
  }

  log.innerHTML = `<div class="thread">${items
    .map((item) => {
      if (item.kind === "user") {
        return `<article class="user">${escapeHtml(item.content)}</article>`;
      }
      if (item.kind === "assistant") {
        return `<article class="assistant">${renderMarkdown(item.content)}</article>`;
      }
      if (item.kind === "note") {
        return `<article class="note">${escapeHtml(item.content)}</article>`;
      }
      if (item.kind === "error") {
        return `<article class="error">${escapeHtml(item.content)}</article>`;
      }
      const state = item.pending ? "executando" : item.timed_out ? "tempo esgotado" : "concluído";
      const output = item.pending
        ? "<p class=\"note\">Rodando na sandbox…</p>"
        : `<pre>${escapeHtml(item.output || "")}</pre>`;
      return `
        <details class="tool" open>
          <summary><span class="name">${escapeHtml(item.name)}</span> · ${state}</summary>
          <div class="body">
            <pre>${escapeHtml(item.arguments || "")}</pre>
            ${output}
          </div>
        </details>`;
    })
    .join("")}</div>`;
  log.scrollTop = log.scrollHeight;
}

function renderFileChip() {
  if (!attachment) {
    fileChip.hidden = true;
    fileChip.innerHTML = "";
    return;
  }
  fileChip.hidden = false;
  fileChip.innerHTML = `<span>${escapeHtml(attachment.name)}</span><button type="button" id="clear-file">remover</button>`;
}

function sendUser(displayText) {
  const display = displayText.trim();
  if ((!display && !attachment) || busy || !socket || socket.readyState !== WebSocket.OPEN) return;
  let content = display;
  if (attachment) {
    const block = `Arquivo ${attachment.name}:\n\`\`\`\n${attachment.text}\n\`\`\``;
    content = display ? `${display}\n\n${block}` : block;
  }
  const shown = display || attachment.name;
  items.push({ kind: "user", content: shown });
  renderThread();
  setBusy(true);
  socket.send(JSON.stringify({ type: "user", content, display: shown }));
  input.value = "";
  input.style.height = "auto";
  attachment = null;
  renderFileChip();
}

function connect() {
  const protocol = location.protocol === "https:" ? "wss" : "ws";
  socket = new WebSocket(`${protocol}://${location.host}/ws`);
  setConn("conectando", false);
  socket.addEventListener("open", () => setConn("no ar", true));
  socket.addEventListener("close", () => {
    setConn("reconectando", false);
    setBusy(false);
    setTimeout(connect, 1200);
  });
  socket.addEventListener("message", (event) => {
    const message = JSON.parse(event.data);
    if (message.type === "snapshot") {
      currentId = message.current_id;
      sessions = message.sessions;
      items = message.items;
      modelLabel.textContent = message.model;
      setBusy(false);
      renderChats();
      renderThread();
      return;
    }
    if (message.type === "sessions") {
      currentId = message.current_id;
      sessions = message.sessions;
      renderChats();
      return;
    }
    if (message.type === "note") items.push({ kind: "note", content: message.content });
    else if (message.type === "tool_call") {
      items.push({
        kind: "tool",
        id: message.id,
        name: message.name,
        arguments: message.arguments,
        output: "",
        timed_out: false,
        pending: true,
      });
    } else if (message.type === "tool_result") {
      const tool = [...items].reverse().find((item) => item.kind === "tool" && item.id === message.id);
      if (tool) {
        tool.output = message.output;
        tool.timed_out = message.timed_out;
        tool.pending = false;
      }
    } else if (message.type === "assistant") {
      items.push({ kind: "assistant", content: message.content });
    } else if (message.type === "error") {
      items.push({ kind: "error", content: message.content });
    } else if (message.type === "done") setBusy(false);
    renderThread();
  });
}

chats.addEventListener("click", (event) => {
  const open = event.target.closest("[data-open]");
  const remove = event.target.closest("[data-delete]");
  if (remove) {
    if (busy || !socket || socket.readyState !== WebSocket.OPEN) return;
    socket.send(JSON.stringify({ type: "delete", session_id: remove.dataset.delete }));
    return;
  }
  if (open && open.dataset.open !== currentId) {
    if (busy || !socket || socket.readyState !== WebSocket.OPEN) return;
    socket.send(JSON.stringify({ type: "open", session_id: open.dataset.open }));
  }
});

log.addEventListener("click", (event) => {
  const button = event.target.closest("[data-suggestion]");
  if (!button || busy) return;
  sendUser(suggestions[Number(button.dataset.suggestion)]);
});

form.addEventListener("submit", (event) => {
  event.preventDefault();
  sendUser(input.value);
});

input.addEventListener("keydown", (event) => {
  if (event.key === "Enter" && !event.shiftKey) {
    event.preventDefault();
    form.requestSubmit();
  }
});

input.addEventListener("input", () => {
  input.style.height = "auto";
  input.style.height = `${Math.min(input.scrollHeight, 180)}px`;
});

newChat.addEventListener("click", () => {
  if (busy || !socket || socket.readyState !== WebSocket.OPEN) return;
  socket.send(JSON.stringify({ type: "new" }));
});

attach.addEventListener("click", () => fileInput.click());

fileInput.addEventListener("change", () => {
  const file = fileInput.files[0];
  fileInput.value = "";
  if (!file) return;
  if (file.size > 60_000) {
    items.push({ kind: "error", content: "O anexo passa de 60 KB." });
    renderThread();
    return;
  }
  const reader = new FileReader();
  reader.onload = () => {
    attachment = { name: file.name, text: String(reader.result) };
    renderFileChip();
  };
  reader.readAsText(file);
});

fileChip.addEventListener("click", (event) => {
  if (event.target.id !== "clear-file") return;
  attachment = null;
  renderFileChip();
});

const SpeechRecognition = window.SpeechRecognition || window.webkitSpeechRecognition;
if (SpeechRecognition) {
  mic.hidden = false;
  recognition = new SpeechRecognition();
  recognition.lang = "pt-BR";
  recognition.onresult = (event) => {
    const text = event.results[0][0].transcript;
    input.value = input.value ? `${input.value} ${text}` : text;
    input.dispatchEvent(new Event("input"));
  };
  recognition.onend = () => {
    listening = false;
    mic.textContent = "mic";
  };
  mic.addEventListener("click", () => {
    if (listening) {
      recognition.stop();
      return;
    }
    listening = true;
    mic.textContent = "…";
    recognition.start();
  });
}

renderThread();
connect();
