// Quick hover explanations for every control. Any element with a `title` (or
// `data-tip`) gets a styled bubble after a short pause, instead of the slow
// native tooltip. The title is moved into `data-tip` while hovering so the
// native one doesn't also appear, and put back afterwards.

const DELAY_MS = 350;

export function installHoverTips(): () => void {
  const bubble = document.createElement("div");
  bubble.className = "hover-tip";
  bubble.setAttribute("role", "tooltip");
  document.body.appendChild(bubble);

  let target: HTMLElement | null = null;
  let timer: number | undefined;

  const tipOf = (el: HTMLElement) => el.getAttribute("data-tip") ?? "";

  function hide() {
    window.clearTimeout(timer);
    bubble.classList.remove("show");
    if (target?.hasAttribute("data-tip-from-title")) {
      target.setAttribute("title", tipOf(target));
      target.removeAttribute("data-tip");
      target.removeAttribute("data-tip-from-title");
    }
    target = null;
  }

  function place(el: HTMLElement) {
    const r = el.getBoundingClientRect();
    const b = bubble.getBoundingClientRect();
    const gap = 8;
    let top = r.bottom + gap;
    if (top + b.height > window.innerHeight - 4) top = r.top - b.height - gap;
    let left = r.left + r.width / 2 - b.width / 2;
    left = Math.max(6, Math.min(left, window.innerWidth - b.width - 6));
    bubble.style.top = `${Math.max(4, top)}px`;
    bubble.style.left = `${left}px`;
  }

  function over(e: MouseEvent) {
    const el = (e.target as HTMLElement | null)?.closest<HTMLElement>("[title],[data-tip]");
    if (el === target) return;
    hide();
    if (!el) return;
    const title = el.getAttribute("title");
    if (title) {
      el.setAttribute("data-tip", title);
      el.setAttribute("data-tip-from-title", "");
      el.removeAttribute("title");
    }
    const text = tipOf(el);
    if (!text.trim()) return;
    target = el;
    timer = window.setTimeout(() => {
      if (target !== el || !el.isConnected) return;
      bubble.textContent = text;
      bubble.classList.add("show");
      place(el);
    }, DELAY_MS);
  }

  document.addEventListener("mouseover", over);
  document.addEventListener("mousedown", hide, true);
  document.addEventListener("keydown", hide, true);
  window.addEventListener("scroll", hide, true);
  window.addEventListener("blur", hide);
  return () => {
    hide();
    document.removeEventListener("mouseover", over);
    document.removeEventListener("mousedown", hide, true);
    document.removeEventListener("keydown", hide, true);
    window.removeEventListener("scroll", hide, true);
    window.removeEventListener("blur", hide);
    bubble.remove();
  };
}
