// The hero demo: type a wrong-layout phrase, then correct it word by word,
// the same way the app does — at the space.

const PHRASES = [
  { wrong: "ghbdtn rfr ltkf", right: "привет как дела" },
  { wrong: "cgfcb,j pfdnhf", right: "спасибо завтра" },
  { wrong: "руддщ ,ednhb", right: "hello внутри" },
  { wrong: "yjhv xedfr", right: "норм чувак" },
];

const el = document.getElementById("text");
const line = document.getElementById("line");
const reduced = window.matchMedia("(prefers-reduced-motion: reduce)").matches;

const wait = (ms) => new Promise((r) => setTimeout(r, ms));

async function typeOut(text) {
  el.textContent = "";
  for (const ch of text) {
    el.textContent += ch;
    await wait(58 + Math.random() * 55);
  }
}

async function run() {
  // Respect a reduced-motion preference: show the finished result, no typing.
  if (reduced) {
    el.textContent = PHRASES[0].right;
    return;
  }

  let index = 0;
  for (;;) {
    const { wrong, right } = PHRASES[index % PHRASES.length];
    await typeOut(wrong);
    await wait(430);

    // Correct one word at a time, as pressing space would.
    const wrongWords = wrong.split(" ");
    const rightWords = right.split(" ");
    for (let i = 0; i < wrongWords.length; i++) {
      const fixed = rightWords.slice(0, i + 1).join(" ");
      const rest = wrongWords.slice(i + 1).join(" ");
      el.textContent = rest ? `${fixed} ${rest}` : fixed;
      line.classList.remove("flash");
      void line.offsetWidth; // restart the animation
      line.classList.add("flash");
      await wait(520);
    }

    await wait(1900);
    index++;
  }
}

run();
