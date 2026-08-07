import { defineUnlistedScript } from '#imports';
import TurndownService from 'turndown';
import { gfm } from '@joplin/turndown-plugin-gfm';
import { Readability } from '@mozilla/readability';

interface PageMarkdownParams {
  selector?: string;
  full?: boolean;
}

/**
 * 页面级 Markdown 转换器（unlisted script）：
 * 由 background 用 chrome.scripting.executeScript({ files: ['page-markdown.js'] })
 * 按需注入到目标标签页，注入后在隔离世界定义 __bridgePageMarkdown 供后续调用。
 * 转换核心使用开源 Turndown + 官方 GFM 插件（表格/删除线/任务列表）。
 */
function makePageMarkdownConverter(): (params: PageMarkdownParams) => unknown {
  const service = new TurndownService({
    headingStyle: 'atx',
    hr: '---',
    bulletListMarker: '-',
    codeBlockStyle: 'fenced',
    fence: '```',
    emDelimiter: '*',
    strongDelimiter: '**',
    linkStyle: 'inlined',
  });
  service.use(gfm);

  const isHidden = (el: Element): boolean => {
    if (el instanceof HTMLElement) {
      if (el.hidden || el.getAttribute('aria-hidden') === 'true') return true;
      const cls = typeof el.className === 'string' ? el.className : '';
      if (/sr[-_]?only|visually[-_]?hidden|screen[-_]?reader/i.test(cls)) return true;
      if (el.style.display === 'none' || el.style.visibility === 'hidden') return true;
      try {
        const cs = getComputedStyle(el);
        if (cs.display === 'none' || cs.visibility === 'hidden') return true;
      } catch {
        // ignore
      }
    }
    return false;
  };

  // 非正文元素直接移除（脚本/样式/表单控件/媒体等）
  service.remove([
    'script',
    'style',
    'noscript',
    'template',
    'iframe',
    'object',
    'embed',
    'canvas',
    'svg',
    'audio',
    'video',
    'source',
    'track',
    'map',
    'area',
    'input',
    'button',
    'select',
    'textarea',
  ]);
  // 隐藏元素移除（规则后加先执行，覆盖前面 remove 的规则）
  service.addRule('remove-hidden', {
    filter: (node) => node instanceof Element && isHidden(node),
    replacement: () => '',
  });
  // 链接补成绝对 URL（覆盖 Turndown 默认行为）
  service.addRule('absolute-links', {
    filter: 'a',
    replacement: (content, node) => {
      const href = node.getAttribute('href');
      const text = content.trim();
      if (!href || !text) return text;
      let url = href;
      try {
        url = new URL(href, document.baseURI).href;
      } catch {
        // 保留原始 href
      }
      const title = node.getAttribute('title');
      const titlePart = title ? ` "${title.replace(/"/g, '\\"')}"` : '';
      return `[${text}](${url
        .replace(/\(/g, '%28')
        .replace(/\)/g, '%29')
        .replace(/ /g, '%20')}${titlePart})`;
    },
  });
  // 图片补成绝对 URL
  service.addRule('absolute-images', {
    filter: 'img',
    replacement: (_content, node) => {
      const src = node.getAttribute('src');
      if (!src) return '';
      let url = src;
      try {
        url = new URL(src, document.baseURI).href;
      } catch {
        // 保留原始 src
      }
      const alt = (node.getAttribute('alt') ?? '').trim();
      const title = node.getAttribute('title');
      const titlePart = title ? ` "${title.replace(/"/g, '\\"')}"` : '';
      return `![${alt}](${url
        .replace(/\(/g, '%28')
        .replace(/\)/g, '%29')
        .replace(/ /g, '%20')}${titlePart})`;
    },
  });

  return (params) => {
    try {
      const selector =
        typeof params?.selector === 'string' && params.selector.trim()
          ? params.selector.trim()
          : '';
      const full = params?.full === true;
      let source: HTMLElement | Document | string;
      if (selector) {
        let el: Element | null = null;
        try {
          el = document.querySelector(selector);
        } catch {
          throw new Error(`invalid selector: ${selector}`);
        }
        if (!el) throw new Error(`selector not found: ${selector}`);
        source = el as HTMLElement;
      } else if (full) {
        if (!document.body) throw new Error('page has no body');
        source = document.body;
      } else {
        // 自动正文提取：Readability（Firefox 阅读模式同款）在 DOM 副本上抽取主内容，
        // 抽不到或内容太少时退回整页转换
        let article: { content: string; textContent: string } | null = null;
        try {
          article = new Readability(document.cloneNode(true) as Document).parse();
        } catch {
          article = null;
        }
        if (article && (article.textContent ?? '').trim().length >= 50) {
          source = article.content;
        } else {
          if (!document.body) throw new Error('page has no body');
          source = document.body;
        }
      }
      const markdown = service
        .turndown(source as TurndownService.Node | string)
        .trim();
      return {
        title: document.title ?? '',
        url: location.href,
        markdown,
      };
    } catch (err) {
      // 把页面内错误转成干净的字符串，避免被 executeScript 的包装错误吞掉细节
      return { __bridge_error: err instanceof Error ? err.message : String(err) };
    }
  };
}

export default defineUnlistedScript(() => {
  (globalThis as Record<string, unknown>).__bridgePageMarkdown =
    makePageMarkdownConverter();
});
