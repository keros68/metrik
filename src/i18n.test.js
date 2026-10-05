import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import test from "node:test";
import { parse } from "@babel/parser";

import en from "./locales/en.js";
import { localeIsChinese, resolveLanguage, setLanguageSetting, t } from "./i18n.js";

// 守卫扫描 src/ 下全部 .js / .jsx（测试文件和 locales/ 除外），新文件自动纳入。
const SOURCE_FILES = readdirSync(new URL("./", import.meta.url), { recursive: true })
  .map((file) => file.replaceAll("\\", "/"))
  .filter((file) => /\.jsx?$/.test(file) && !file.endsWith(".test.js") && !file.startsWith("locales/"))
  .sort()
  .map((file) => `src/${file}`);

const CJK = /[　-〿㐀-䶿一-鿿豈-﫿＀-￯]/;
const PLACEHOLDER = /\{(\w+)\}/g;
const IGNORE_MARK = "i18n-ignore";

function placeholders(text) {
  return [...new Set([...text.matchAll(PLACEHOLDER)].map((match) => match[1]))].sort();
}

// 返回违规列表：带 CJK 的字符串字面量必须有英文条目；JSX 文本、JSX 属性字面量、
// 带 ${} 的模板字符串里不得出现 CJK（必须经 t()）。注释不参与检查；行尾或上一行
// 写 `i18n-ignore` 注释的字面量豁免（只用于不是界面文字的中文，例如语言名）。
export function findI18nViolations(source, dictionary, file = "<source>") {
  const ast = parse(source, {
    sourceType: "module",
    plugins: ["jsx"],
    errorRecovery: false,
  });
  const lines = source.split(/\r?\n/);
  const ignoredLines = new Set();
  for (const comment of ast.comments || []) {
    if (!comment.value.includes(IGNORE_MARK)) continue;
    const { line, column } = comment.loc.start;
    const standalone = lines[line - 1].slice(0, column).trim() === "";
    // 独占一行的标记豁免下一行；行尾标记只豁免本行。
    ignoredLines.add(standalone ? comment.loc.end.line + 1 : line);
  }
  const violations = [];
  const report = (node, message) => {
    if (ignoredLines.has(node.loc.start.line)) return;
    violations.push(`${file}:${node.loc.start.line}: ${message}`);
  };
  const needsEntry = (node, text) => {
    if (!Object.prototype.hasOwnProperty.call(dictionary, text)) {
      report(node, `missing English entry for ${JSON.stringify(text)}`);
    }
  };

  const visit = (node, parent) => {
    if (!node || typeof node.type !== "string") return;
    switch (node.type) {
      case "JSXText":
        if (CJK.test(node.value)) report(node, `raw CJK JSX text ${JSON.stringify(node.value.trim())}; wrap it in t()`);
        break;
      case "StringLiteral":
        if (CJK.test(node.value)) {
          if (parent?.type === "JSXAttribute") {
            report(node, `raw CJK JSX attribute ${JSON.stringify(node.value)}; use {t(...)}`);
          } else {
            needsEntry(node, node.value);
          }
        }
        break;
      case "TemplateLiteral": {
        const cjk = node.quasis.some((quasi) => CJK.test(quasi.value.cooked ?? quasi.value.raw));
        if (cjk && node.expressions.length > 0) {
          report(node, "CJK inside a template literal with ${}; use t() with {placeholders}");
        } else if (cjk) {
          needsEntry(node, node.quasis[0].value.cooked);
        }
        break;
      }
      default:
        break;
    }
    for (const key of Object.keys(node)) {
      if (key === "loc" || key === "start" || key === "end" || key === "extra"
        || key === "comments" || key === "leadingComments" || key === "trailingComments"
        || key === "innerComments") continue;
      const value = node[key];
      if (Array.isArray(value)) {
        for (const child of value) visit(child, node);
      } else if (value && typeof value.type === "string") {
        visit(value, node);
      }
    }
  };
  visit(ast.program, null);
  return violations;
}

test("source files route every CJK string through t() with an English entry", () => {
  assert.ok(SOURCE_FILES.includes("src/App.jsx"), SOURCE_FILES.join(", "));
  const violations = SOURCE_FILES.flatMap((file) =>
    findI18nViolations(readFileSync(new URL(`../${file}`, import.meta.url), "utf8"), en, file));
  assert.deepEqual(violations, []);
});

test("the guard catches untranslated text and honors comments and the ignore mark", () => {
  const source = [
    "// 注释里的中文不检查",
    "const LABELS = [{ label: \"已翻译\" }, { label: \"未翻译\" }];",
    "const name = \"简体中文\"; // i18n-ignore",
    "const tpl = `共 ${count} 个`;",
    "export const A = () => <p title=\"提示\">直接写的中文 {t(\"已翻译\")}</p>;",
  ].join("\n");
  const violations = findI18nViolations(source, { "已翻译": "Translated" });
  assert.equal(violations.length, 4, violations.join("\n"));
  assert.match(violations[0], /:2: missing English entry for "未翻译"/);
  assert.match(violations[1], /:4: CJK inside a template literal/);
  assert.match(violations.join("\n"), /:5: raw CJK JSX attribute "提示"/);
  assert.match(violations.join("\n"), /:5: raw CJK JSX text "直接写的中文"/);
});

test("every plain English entry keeps the placeholder names of its key", () => {
  const mismatched = Object.entries(en)
    .filter(([key, value]) => typeof value === "string"
      && placeholders(key).join(",") !== placeholders(value).join(","))
    .map(([key, value]) => `${key} -> ${value}`);
  assert.deepEqual(mismatched, []);
  for (const [key, value] of Object.entries(en)) {
    assert.ok(typeof value === "string" || typeof value === "function", `bad entry type for ${key}`);
  }
});

test("auto follows the primary language subtag of the system locale", () => {
  for (const locale of ["zh-CN", "zh_CN.UTF-8", "zh-TW", "zh-Hans-CN"]) {
    assert.equal(resolveLanguage("auto", locale), "zh", locale);
    assert.equal(localeIsChinese(locale), true, locale);
  }
  for (const locale of ["en-US", "de-DE", "C", "POSIX", "", undefined, null]) {
    assert.equal(resolveLanguage("auto", locale), "en", String(locale));
  }
  assert.equal(resolveLanguage("zh", "en-US"), "zh");
  assert.equal(resolveLanguage("en", "zh-CN"), "en");
});

test("t() returns the Chinese key in zh and the English entry in en", async () => {
  await setLanguageSetting("zh");
  assert.equal(t("语言"), "语言");
  assert.equal(t("{count} 张", { count: 3 }), "3 张");
  await setLanguageSetting("en");
  assert.equal(t("语言"), "Language");
  assert.equal(t("{count} 张", { count: 1 }), "1 credit");
  assert.equal(t("{count} 张", { count: 3 }), "3 credits");
  assert.equal(t("没有英文条目的 {name}", { name: "x" }), "没有英文条目的 x");
  await setLanguageSetting("zh");
});
