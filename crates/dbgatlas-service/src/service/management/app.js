"use strict";
const $ = (id) => document.getElementById(id);
const state = {
  project: "",
  workspaceRoot: undefined,
  session: "",
  offset: 0,
  data: null,
  selected: "",
  artifact: null,
  outputOffset: 0,
  querySeq: 0,
  outputSeq: 0,
};
const labels = {
  success: "成功",
  failed: "失败",
  running: "运行中",
  pending: "等待中",
  canceled: "已取消",
  unknown: "未知",
  active: "活动",
  closed: "已关闭",
  opening: "打开中",
  closing: "关闭中",
};
const ref = (value) => (typeof value === "string" ? value : value?.id || "");
const time = (value) =>
  value?.unix_millis
    ? new Date(value.unix_millis).toLocaleString("zh-CN", { hour12: false })
    : "—";
const node = (tag, text, className) => {
  const item = document.createElement(tag);
  if (text !== undefined) item.textContent = text;
  if (className) item.className = className;
  return item;
};
const badge = (status) =>
  node("span", labels[status] || status || "未知", "badge " + status);
function error(message) {
  $("error").textContent = message || "";
  $("error").hidden = !message;
}
function recent(projects, roots) {
  const select = $("recent");
  select.replaceChildren(node("option", "选择项目…"));
  select.firstChild.value = "";
  const duplicates = new Set(
    projects.filter((v, i) => projects.indexOf(v) !== i),
  );
  roots.forEach((root, i) => {
    const option = node(
      "option",
      duplicates.has(projects[i]) ? root : projects[i],
    );
    option.value = root;
    option.dataset.project = projects[i];
    select.append(option);
  });
  select.value = state.workspaceRoot || "";
}
async function query(params) {
  const response = await fetch("/api/query", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(params),
  });
  const value = await response.json();
  if (!response.ok || value.error) throw Error(value.error || "查询失败");
  return value;
}
async function health() {
  try {
    const value = await query({ action: "overview" });
    $("health").textContent = "服务在线";
    $("health-dot").classList.add("online");
    $("version").textContent = "v" + value.version;
    recent(value.projects, value.workspace_roots);
    if (!state.project && value.workspace_roots.length) {
      $("project").value = value.projects[0];
      openProject(value.workspace_roots[0]);
    }
  } catch (e) {
    $("health").textContent = "连接失败";
    $("health-dot").classList.remove("online");
    error(e.message);
  }
}
function command(op) {
  const input = op.input || {};
  return input.command || input.commands?.join("\n") || op.summary || "—";
}
function target(session) {
  const request = session.target?.open_request || {};
  return (
    request.executable?.split(/[\\/]/).pop() ||
    request.path?.split(/[\\/]/).pop() ||
    request.program ||
    (session.target?.process?.pid
      ? "PID " + session.target.process.pid
      : session.kind)
  );
}
function hideDetail() {
  state.outputSeq++;
  state.artifact = null;
  state.selected = "";
  $("detail").hidden = true;
}
async function load(reset = false) {
  if (!state.project) return;
  if (reset) state.offset = 0;
  const sequence = ++state.querySeq;
  $("refresh").disabled = true;
  error("");
  try {
    const data = await query({
      action: "workspace",
      project_root: state.project,
      workspace_root: state.workspaceRoot,
      session_id: state.session,
      status: $("status").value,
      search: $("search").value,
      start_ms: $("time-start").value
        ? new Date($("time-start").value).getTime()
        : undefined,
      end_ms: $("time-end").value
        ? new Date($("time-end").value).getTime()
        : undefined,
      offset: state.offset,
      limit: 40,
    });
    if (sequence !== state.querySeq) return;
    state.data = data;
    state.offset = data.offset;
    render();
    history.replaceState(
      null,
      "",
      "/?project=" +
        encodeURIComponent(state.project) +
        (state.workspaceRoot
          ? "&workspace=" + encodeURIComponent(state.workspaceRoot)
          : ""),
    );
    $("updated").textContent =
      "更新于 " + new Date().toLocaleTimeString("zh-CN", { hour12: false });
  } catch (e) {
    if (sequence === state.querySeq) {
      error(e.message);
      if (!state.data) {
        $("records-caption").textContent = "未能打开工作区";
        $("empty").replaceChildren(
          node("h3", "无法读取这个项目"),
          node("p", "检查项目目录是否包含已有的 dbgatlas 工作区，然后重试。"),
        );
        $("page-label").textContent = "没有已加载的记录";
      }
    }
  } finally {
    if (sequence === state.querySeq) $("refresh").disabled = false;
  }
}
function render() {
  const data = state.data;
  $("session-count").textContent = data.sessions.length;
  $("session-total").textContent = data.sessions.length;
  $("operation-count").textContent = data.total_operations;
  $("artifact-count").textContent = data.total_artifacts;
  $("records-title").textContent = state.session ? "会话操作" : "全部操作";
  $("records-caption").textContent = state.session || data.workspace.root;
  $("all-sessions").classList.toggle("selected", !state.session);
  $("warning").hidden = !data.warnings.length;
  $("warning").textContent = data.warnings.join("；");
  $("session-list").replaceChildren();
  for (const session of data.sessions) {
    const sid = ref(session.session_id);
    const button = node(
      "button",
      undefined,
      "session-item" + (state.session === sid ? " selected" : ""),
    );
    button.append(node("strong", target(session)));
    button.append(
      node(
        "small",
        (labels[session.lifecycle] || session.lifecycle) +
          " · " +
          (session.current_service ? "本次服务" : "历史记录") +
          "\n" +
          sid,
      ),
    );
    button.title = sid + "\n" + time(session.created_at);
    button.addEventListener("click", () => {
      state.session = sid;
      hideDetail();
      load(true);
    });
    $("session-list").append(button);
  }
  if (!data.sessions.length)
    $("session-list").append(
      node("div", "没有会话记录，仍可查看独立操作。", "empty small"),
    );
  $("operations").replaceChildren();
  for (const op of data.operations) {
    const id = ref(op.operation_id);
    const tr = node("tr");
    tr.tabIndex = 0;
    tr.setAttribute("aria-label", "查看 " + op.capability + " " + id);
    tr.classList.toggle("selected", state.selected === id);
    const action = node("td");
    action.append(
      node("span", op.capability, "op-name"),
      node("span", command(op), "op-command"),
    );
    action.title = command(op);
    const status = node("td");
    status.append(badge(op.status));
    tr.append(
      action,
      status,
      node("td", time(op.created_at), "record-time"),
      node("td", "↗"),
    );
    const select = () => showDetail(op);
    tr.addEventListener("click", select);
    tr.addEventListener("keydown", (event) => {
      if (event.key === "Enter") {
        event.preventDefault();
        select();
      }
    });
    $("operations").append(tr);
  }
  $("empty").hidden = !!data.operations.length;
  if (!data.operations.length) {
    $("empty").replaceChildren(
      node("div", "⌘", "empty-icon"),
      node(
        "h3",
        data.total_operations ? "没有匹配的操作" : "这个工作区还没有操作记录",
      ),
      node(
        "p",
        data.total_operations
          ? "调整会话、搜索词或状态筛选后重试。"
          : "通过 MCP 或 CLI 调试后，点击刷新即可查看。",
      ),
    );
  }
  $("page-label").textContent = data.filtered_count
    ? `${data.offset + 1}–${data.offset + data.operations.length} / ${data.filtered_count} 条`
    : "0 条记录";
  $("previous").disabled = !data.offset;
  $("next").disabled = !data.has_more;
}
function showDetail(op) {
  hideDetail();
  state.selected = ref(op.operation_id);
  $("execution-history").open = false;
  $("history-content").textContent = "";
  $("history-more").hidden = true;
  $("provenance").textContent = "";
  $("detail").hidden = false;
  $("detail-title").textContent = op.capability;
  $("detail-id").textContent = state.selected;
  $("detail-summary").replaceChildren(
    badge(op.status),
    node(
      "span",
      op.summary + (op.output_error ? " · 输出问题：" + op.output_error : ""),
    ),
  );
  $("command").textContent = command(op);
  $("context").textContent = JSON.stringify(
    {
      session_id: op.session_id,
      input: op.input,
      execution_context: op.execution_context,
    },
    null,
    2,
  );
  const ids = new Set(
    [...(op.artifacts || []).map(ref), ref(op.raw_output)].filter(Boolean),
  );
  const artifacts = state.data.artifacts.filter((a) =>
    ids.has(ref(a.artifact_id)),
  );
  $("artifacts").replaceChildren();
  $("more").hidden = true;
  $("output").textContent = "选择左侧产物查看内容";
  $("output-meta").textContent = "";
  $("output-note").textContent = "";
  for (const a of artifacts) {
    const button = node("button", a.relative_path, "artifact-item");
    button.append(node("small", a.kind + " · " + ref(a.artifact_id)));
    button.addEventListener("click", () => openArtifact(a));
    $("artifacts").append(button);
  }
  if (!artifacts.length) {
    $("artifacts").append(node("span", "尚无已登记产物", "project-hint"));
    $("output").textContent = "此操作尚无已登记输出。运行中的操作可稍后刷新。";
  } else {
    const initial =
      artifacts.find((a) => ref(a.artifact_id) === ref(op.raw_output)) ||
      artifacts.find((a) =>
        /\.(txt|log|md|json|jsonl|csv)$/i.test(a.relative_path),
      );
    if (initial) openArtifact(initial);
  }
  render();
  $("detail").scrollIntoView({ behavior: "smooth", block: "start" });
}
let historyAfter = 0;
async function loadHistory(append) {
  const id = state.selected,
    project = state.project,
    seq = state.querySeq;
  if (!id) return;
  $("history-more").disabled = true;
  try {
    const result = await query({
      action: "history",
      project_root: project,
      workspace_root: state.workspaceRoot,
      execution_id: id,
      after: append ? historyAfter : 0,
    });
    if (id !== state.selected || seq !== state.querySeq) return;
    const lines = result.entries.map(
      (e) =>
        new Date(e.observed).toLocaleString() +
        " · " +
        e.record.status +
        " · " +
        e.record.summary +
        (e.record.output_error ? " · " + e.record.output_error : ""),
    );
    $("history-content").textContent =
      (append ? $("history-content").textContent + "\n" : "") +
      lines.join("\n");
    historyAfter = result.entries.at(-1)?.sequence || 0;
    $("history-more").hidden = result.entries.length < 100;
  } catch (e) {
    if (id === state.selected && seq === state.querySeq)
      $("history-content").textContent = e.message;
  } finally {
    if (id === state.selected && seq === state.querySeq)
      $("history-more").disabled = false;
  }
}
$("execution-history").addEventListener("toggle", () => {
  if ($("execution-history").open) loadHistory(false);
});
$("history-more").addEventListener("click", () => loadHistory(true));
async function showProvenance(artifact, seq) {
  $("provenance").textContent = "";
  try {
    const v = await query({
      action: "provenance",
      project_root: state.project,
      workspace_root: state.workspaceRoot,
      artifact_id: artifact.artifact_id,
    });
    if (seq !== state.outputSeq) return;
    const source = [
      ref(v.artifact.producer_session_id),
      ref(v.artifact.producer_execution_id),
    ]
      .filter(Boolean)
      .join(" / ");
    $("provenance").textContent =
      "来源：" +
      (source || "无已知生产者") +
      "；引用会话：" +
      (v.consumer_sessions.map(ref).join("、") || "无");
  } catch (e) {
    if (seq === state.outputSeq) $("provenance").textContent = e.message;
  }
}
function openArtifact(artifact) {
  state.artifact = artifact;
  state.outputOffset = 0;
  state.outputSeq++;
  $("output").textContent = "正在读取…";
  $("output-meta").textContent = artifact.relative_path;
  $("output-note").textContent = "";
  $("more").hidden = true;
  showProvenance(artifact, state.outputSeq);
  readMore(false);
}
async function readMore(append) {
  const seq = state.outputSeq;
  const artifact = state.artifact;
  if (!artifact) return;
  $("more").disabled = true;
  try {
    const value = await query({
      action: "output",
      project_root: state.project,
      workspace_root: state.workspaceRoot,
      artifact_id: artifact.artifact_id,
      offset: state.outputOffset,
    });
    if (seq !== state.outputSeq) return;
    if (value.binary) {
      $("output").textContent =
        "此产物为二进制或暂不支持预览的格式。\n\n" + artifact.relative_path;
      $("output-note").textContent = "文件保留在工作区中";
      return;
    }
    $("output").textContent =
      (append ? $("output").textContent : "") + value.text;
    state.outputOffset = value.next_offset;
    $("output-note").textContent =
      `已读取 ${value.next_offset.toLocaleString()} / ${value.byte_len.toLocaleString()} 字节` +
      (value.lossy ? " · 包含无法按 UTF-8 显示的字节" : "");
    $("more").hidden = !value.has_more;
  } catch (e) {
    if (seq === state.outputSeq) {
      $("output").textContent = "无法读取输出：" + e.message;
      $("output-note").textContent = "请确认文件仍位于原工作区中";
    }
  } finally {
    if (seq === state.outputSeq) $("more").disabled = false;
  }
}
function openProject(workspaceRoot) {
  const root = $("project").value.trim();
  if (!root) return;
  state.project = root;
  state.workspaceRoot = workspaceRoot;
  state.session = "";
  hideDetail();
  state.data = null;
  $("operations").replaceChildren();
  $("session-list").replaceChildren();
  for (const id of ["session-count", "operation-count", "artifact-count"])
    $(id).textContent = "—";
  $("session-total").textContent = "0";
  $("records-title").textContent = "操作记录";
  $("records-caption").textContent = "正在打开 " + root;
  $("previous").disabled = true;
  $("next").disabled = true;
  $("empty").hidden = false;
  $("empty").replaceChildren(node("h3", "正在读取工作区…"));
  $("page-label").textContent = "等待查询结果";
  $("warning").hidden = true;
  load(true);
}
$("project-form").addEventListener("submit", (e) => {
  e.preventDefault();
  openProject();
});
$("recent").addEventListener("change", (e) => {
  if (e.target.value) {
    $("project").value = e.target.selectedOptions[0].dataset.project;
    openProject(e.target.value);
  }
});
$("all-sessions").addEventListener("click", () => {
  state.session = "";
  hideDetail();
  load(true);
});
$("refresh").addEventListener("click", () => {
  hideDetail();
  health();
  load();
});
$("status").addEventListener("change", () => {
  hideDetail();
  load(true);
});
for (const id of ["time-start", "time-end"])
  $(id).addEventListener("change", () => {
    hideDetail();
    load(true);
  });
let timer;
$("search").addEventListener("input", () => {
  clearTimeout(timer);
  timer = setTimeout(() => {
    hideDetail();
    load(true);
  }, 250);
});
$("previous").addEventListener("click", () => {
  state.offset = Math.max(0, state.offset - 40);
  hideDetail();
  load();
});
$("next").addEventListener("click", () => {
  state.offset += 40;
  hideDetail();
  load();
});
$("close-detail").addEventListener("click", () => {
  hideDetail();
  render();
});
$("more").addEventListener("click", () => readMore(true));
health();
const initial = new URLSearchParams(location.search).get("project");
if (initial) {
  $("project").value = initial;
  openProject(
    new URLSearchParams(location.search).get("workspace") || undefined,
  );
}
