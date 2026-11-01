import * as path from 'path';
import { Node, Edge, ExtractionResult, ExtractionError, UnresolvedReference, EdgeKind } from '../types';
import { generateNodeId } from './tree-sitter-helpers';

interface HeadingInfo {
  id: string;
  level: number;
  title: string;
  slug: string;
  line: number;
  endLine: number;
  node: Node;
}

interface FenceState {
  marker: string;
  language: string;
  startLine: number;
  lines: Array<{ text: string; line: number }>;
}

/** Extraction inputs the Markdown extractor needs beyond the file itself. */
export interface MarkdownExtractorContext {
  /**
   * Basenames of every file the index contains. Markdown path mentions and
   * links resolve through the file-path arm, which looks candidates up by
   * basename; a target whose basename is absent cannot resolve to a file.
   * Omitted by callers without an index (unit tests, single-file tooling),
   * which keeps the old permissive behaviour.
   */
  knownFileNames?: ReadonlySet<string>;
  /**
   * Skip the per-link import node for a target whose basename no indexed file
   * has. Set only for a full project index. The reference is always kept: the
   * resolver still tries the target's route/module/symbol arms and sync can
   * retry a failed reference when a file appears later (#1240). The link node
   * is decoration (a search child and a `contains` edge), so dropping it
   * loses no edge. Without this flag every target gets its node, as before.
   */
  pruneMissingTargets?: boolean;
}

/**
 * Lightweight Markdown extractor.
 *
 * Markdown is indexed as documentation structure rather than code syntax:
 * headings become searchable module nodes, links become import nodes, and
 * shell-like fenced blocks create command nodes plus file-path references.
 */
export class MarkdownExtractor {
  private filePath: string;
  private lines: string[];
  private nodes: Node[] = [];
  private edges: Edge[] = [];
  private unresolvedReferences: UnresolvedReference[] = [];
  private errors: ExtractionError[] = [];
  private headings: HeadingInfo[] = [];
  private referenceKeys = new Set<string>();
  private knownFileNames?: ReadonlySet<string>;
  private pruneMissingTargets: boolean;
  /**
   * Targets whose import node was skipped. Their references are still stored;
   * this list feeds a separate, larger `links:` part of the file digest so a
   * link beyond the display digest's 12-item budget still stays searchable.
   */
  private prunedTargets: string[] = [];
  /** Commands seen per owning section/file, for the searchable summary. */
  private ownerCommands = new Map<string, string[]>();
  /**
   * Text of every folded table row, list item and command, with its owner.
   * The section preview is capped at 600 characters, so a late row or the 9th
   * command would otherwise lose the searchability its old node had; the file
   * digest carries the text its owner preview does not already contain.
   */
  private foldedText: Array<{ ownerId: string; text: string }> = [];

  constructor(filePath: string, source: string, context: MarkdownExtractorContext = {}) {
    this.filePath = normalizeRelativePath(filePath);
    this.lines = source.split('\n');
    this.knownFileNames = context.knownFileNames;
    this.pruneMissingTargets = context.pruneMissingTargets === true;
  }

  extract(): ExtractionResult {
    const startTime = Date.now();

  try {
      const fileNode = this.createFileNode();
      this.extractHeadings(fileNode);
      this.extractStructuredBlocks(fileNode);
      this.extractLinksAndCommands(fileNode);
      this.applyCommandSummaries(fileNode);
      this.finalizeFileDigest(fileNode);
      this.applySearchText(fileNode);
    } catch (error) {
      this.errors.push({
        message: `Markdown extraction error: ${error instanceof Error ? error.message : String(error)}`,
        filePath: this.filePath,
        severity: 'error',
        code: 'parse_error',
      });
    }

    return {
      nodes: this.nodes,
      edges: this.edges,
      unresolvedReferences: this.unresolvedReferences,
      errors: this.errors,
      durationMs: Date.now() - startTime,
    };
  }

  private createFileNode(): Node {
    const id = generateNodeId(this.filePath, 'file', this.filePath, 1);
    const fileNode: Node = {
      id,
      kind: 'file',
      name: path.posix.basename(this.filePath),
      qualifiedName: this.filePath,
      filePath: this.filePath,
      language: 'markdown',
      startLine: 1,
      endLine: Math.max(1, this.lines.length),
      startColumn: 0,
      endColumn: this.lines[this.lines.length - 1]?.length || 0,
      // docstring is filled in by finalizeFileDigest() once headings and
      // references are known, so it becomes a triage-friendly digest.
      docstring: undefined,
      updatedAt: Date.now(),
    };

    this.nodes.push(fileNode);
    return fileNode;
  }

  private extractHeadings(fileNode: Node): void {
    const rawHeadings: Array<{ level: number; title: string; line: number; column: number }> = [];
    const frontmatterEnd = this.frontmatterEndIndex();
    let fenceMarker: string | null = null;

    for (let i = 0; i < this.lines.length; i++) {
      if (i <= frontmatterEnd) continue;
      const line = this.lines[i]!;

      // Track fenced code blocks so `# foo` comments and `===`/`---` lines
      // inside a code sample are never mistaken for document headings.
      const fence = /^(\s*)(`{3,}|~{3,})/.exec(line);
      if (fence) {
        const marker = fence[2]![0]!.repeat(fence[2]!.length);
        if (fenceMarker === null) fenceMarker = marker;
        else if (line.trimStart().startsWith(fenceMarker)) fenceMarker = null;
        continue;
      }
      if (fenceMarker !== null) continue;

      // ATX heading: `## Title`
      const atx = /^(#{1,6})\s+(.+?)\s*#*\s*$/.exec(line);
      if (atx) {
        rawHeadings.push({
          level: atx[1]!.length,
          title: stripInlineMarkdown(atx[2]!.trim()),
          line: i + 1,
          column: line.indexOf('#'),
        });
        continue;
      }

      // Setext heading: a paragraph line underlined by `===` (level 1) or
      // `---` (level 2). CommonMark treats a `---` directly under a paragraph
      // as a heading, not a thematic break — and the frontmatter skip above
      // keeps a closing `---` from turning its last key into a heading.
      const underline = /^(=+|-+)\s*$/.exec(line.trim());
      if (underline && i - 1 > frontmatterEnd && isSetextTextLine(this.lines[i - 1] ?? '')) {
        rawHeadings.push({
          level: underline[1]![0] === '=' ? 1 : 2,
          title: stripInlineMarkdown((this.lines[i - 1] ?? '').trim()),
          line: i, // the text line carries the heading
          column: 0,
        });
      }
    }

    const slugCounts = new Map<string, number>();
    for (let i = 0; i < rawHeadings.length; i++) {
      const heading = rawHeadings[i]!;
      const baseSlug = slugifyHeading(heading.title);
      const seen = slugCounts.get(baseSlug) ?? 0;
      slugCounts.set(baseSlug, seen + 1);
      const slug = seen === 0 ? baseSlug : `${baseSlug}-${seen}`;

      let endLine = this.lines.length;
      for (let j = i + 1; j < rawHeadings.length; j++) {
        if (rawHeadings[j]!.level <= heading.level) {
          endLine = rawHeadings[j]!.line - 1;
          break;
        }
      }

      const nodeId = generateNodeId(this.filePath, 'module', `${slug}:${heading.line}`, heading.line);
      const node: Node = {
        id: nodeId,
        kind: 'module',
        name: heading.title,
        qualifiedName: `${this.filePath}#${slug}`,
        filePath: this.filePath,
        language: 'markdown',
        signature: `${'#'.repeat(heading.level)} ${heading.title}`,
        docstring: this.buildDocstring(heading.line + 1, endLine),
        startLine: heading.line,
        endLine,
        startColumn: heading.column,
        endColumn: this.lines[heading.line - 1]?.length || 0,
        updatedAt: Date.now(),
      };

      this.nodes.push(node);
      this.headings.push({
        id: nodeId,
        level: heading.level,
        title: heading.title,
        slug,
        line: heading.line,
        endLine,
        node,
      });
    }

    const stack: HeadingInfo[] = [];
    for (const heading of this.headings) {
      while (stack.length > 0 && stack[stack.length - 1]!.level >= heading.level) {
        stack.pop();
      }
      const parent = stack[stack.length - 1];
      this.edges.push({
        source: parent?.id ?? fileNode.id,
        target: heading.id,
        kind: 'contains',
        provenance: 'heuristic',
      });
      stack.push(heading);
    }
  }

  private extractLinksAndCommands(fileNode: Node): void {
    let fence: FenceState | null = null;

    for (let i = 0; i < this.lines.length; i++) {
      const line = this.lines[i]!;
      const lineNumber = i + 1;
      const fenceMatch = /^(\s*)(`{3,}|~{3,})\s*([A-Za-z0-9_+.-]*)/.exec(line);

      if (fence) {
        if (line.trimStart().startsWith(fence.marker)) {
          this.extractCommandsFromFence(fence, fileNode);
          fence = null;
        } else {
          fence.lines.push({ text: line, line: lineNumber });
        }
        continue;
      }

      if (fenceMatch) {
        fence = {
          marker: fenceMatch[2]![0]!.repeat(fenceMatch[2]!.length),
          language: (fenceMatch[3] ?? '').toLowerCase(),
          startLine: lineNumber,
          lines: [],
        };
        continue;
      }

      const owner = this.findOwnerForLine(lineNumber) ?? fileNode;
      this.extractMarkdownLinks(line, lineNumber, owner);
      if (!isTableRowLine(line)) {
        this.extractFileSymbolMentions(line, lineNumber, owner);
        this.extractPathMentions(line, lineNumber, owner);
      }
    }

    if (fence) {
      this.extractCommandsFromFence(fence, fileNode);
    }
  }

  private extractStructuredBlocks(fileNode: Node): void {
    this.extractTables(fileNode);
    this.extractListItems(fileNode);
  }

  private extractTables(fileNode: Node): void {
    let inFence: string | null = null;

    for (let i = 0; i < this.lines.length - 1; i++) {
      const line = this.lines[i]!;
      const fenceMatch = /^(\s*)(`{3,}|~{3,})/.exec(line);
      if (fenceMatch) {
        const marker = fenceMatch[2]!;
        if (inFence === null) {
          inFence = marker;
        } else if (
          marker[0] === inFence[0] &&
          marker.length >= inFence.length &&
          line.slice(fenceMatch[0].length).trim() === ''
        ) {
          inFence = null;
        }
        continue;
      }
      if (inFence) continue;

      const separator = this.lines[i + 1]!;
      if (!isTableHeaderLine(line) || !isTableSeparatorLine(separator)) continue;

      const headers = parseTableCells(line);
      if (headers.length < 2) continue;
      // The header row is search text too: a table past the section preview
      // must keep its column names (a header such as RollbackWindow) findable.
      const headerOwner = this.findOwnerForLine(i + 1) ?? fileNode;
      this.foldedText.push({ ownerId: headerOwner.id, text: headers.join(' | ') });

      let rowIndex = i + 2;
      while (rowIndex < this.lines.length && isTableRowLine(this.lines[rowIndex]!)) {
        const rowLine = this.lines[rowIndex]!;
        const cells = parseTableCells(rowLine);
        const lineNumber = rowIndex + 1;
        if (shouldIndexTableRow(headers, cells)) {
          this.addTableRowReferences(cells, rowLine, lineNumber, fileNode);
        }
        rowIndex++;
      }

      i = rowIndex - 1;
    }
  }

  private extractListItems(fileNode: Node): void {
    let inFence: string | null = null;

    for (let i = 0; i < this.lines.length; i++) {
      const line = this.lines[i]!;
      const fenceMatch = /^(\s*)(`{3,}|~{3,})/.exec(line);
      if (fenceMatch) {
        const marker = fenceMatch[2]!;
        if (inFence === null) {
          inFence = marker;
        } else if (
          marker[0] === inFence[0] &&
          marker.length >= inFence.length &&
          line.slice(fenceMatch[0].length).trim() === ''
        ) {
          inFence = null;
        }
        continue;
      }
      if (inFence) continue;

      const match = /^\s*[-*+]\s+(?:\[[ xX]\]\s+)?(.+?)\s*$/.exec(line);
      if (!match) continue;

      const text = stripInlineMarkdown(match[1]!.trim());
      if (!shouldIndexListItem(text)) continue;

      const lineNumber = i + 1;
      const owner = this.findOwnerForLine(lineNumber) ?? fileNode;
      // The section docstring already carries the item text; its separate
      // `text:` digest part keeps a late item searchable.
      this.foldedText.push({ ownerId: owner.id, text });
      this.extractStructuredReferences(text, lineNumber, owner);
    }
  }

  private addTableRowReferences(
    cells: string[],
    rowLine: string,
    lineNumber: number,
    fileNode: Node
  ): void {
    const owner = this.findOwnerForLine(lineNumber) ?? fileNode;
    // The row text is already part of the section docstring; its separate
    // `text:` digest part keeps a late row searchable.
    const rowText = cells.join(' | ');
    this.foldedText.push({ ownerId: owner.id, text: rowText });
    this.extractStructuredReferences(rowText, lineNumber, owner);
    this.extractCommandsFromStructuredCells(cells, rowLine, lineNumber, owner);
  }

  private extractCommandsFromStructuredCells(cells: string[], rowLine: string, lineNumber: number, owner: Node): void {
    for (const cell of cells) {
      const candidates = extractCodeSpans(cell);
      if (candidates.length === 0) candidates.push(stripInlineMarkdown(cell));

      for (const candidate of candidates) {
        const command = extractCommandFromText(candidate);
        if (!command) continue;
        const column = Math.max(0, rowLine.indexOf(candidate));
        this.addCommand(command, lineNumber, column, owner);
      }
    }
  }

  /**
   * Record a command on its owning section instead of minting a node for it.
   * The command text joins the owner's searchable summary; a script the
   * command runs keeps its `calls` reference, now from the section.
   */
  private addCommand(command: string, line: number, column: number, owner: Node): void {
    // Every recognized command stays in the file's search text; the section
    // summary below is the bounded display form.
    this.foldedText.push({ ownerId: owner.id, text: command });
    const commands = this.ownerCommands.get(owner.id) ?? [];
    if (commands.length < MAX_COMMANDS_PER_OWNER && !commands.includes(command)) {
      commands.push(command);
      this.ownerCommands.set(owner.id, commands);
    }

    const scriptPath = extractScriptPath(command);
    if (!scriptPath) return;
    const normalizedTarget = this.normalizeReference(scriptPath);
    if (!normalizedTarget) return;
    this.addReference(owner.id, normalizedTarget, 'calls', line, column);
  }

  /**
   * Append the per-owner command summary to each section's docstring. Heading
   * docstrings are capped, so a command past the cap would otherwise stop
   * being searchable once its node is gone.
   */
  private applyCommandSummaries(fileNode: Node): void {
    const headingNodes = new Map(this.headings.map((heading) => [heading.id, heading.node]));
    for (const [ownerId, commands] of this.ownerCommands) {
      if (ownerId === fileNode.id || commands.length === 0) continue;
      const owner = headingNodes.get(ownerId);
      if (!owner) continue;
      const summary = summarizeCommands(commands);
      owner.docstring = owner.docstring ? `${owner.docstring}  ·  commands: ${summary}` : `commands: ${summary}`;
    }
  }

  private extractMarkdownLinks(line: string, lineNumber: number, owner: Node): void {
    const linkRegex = /!?\[([^\]]*)\]\(([^)\s]+)(?:\s+["'][^"']*["'])?\)/g;
    let match: RegExpExecArray | null;

    while ((match = linkRegex.exec(line)) !== null) {
      const target = match[2]!;
      if (!isLocalReference(target)) continue;

      const normalizedTarget = this.normalizeReference(target);
      if (!normalizedTarget) continue;

      const column = match.index;
      if (this.shouldCreateLinkNode(normalizedTarget)) {
        const label = stripInlineMarkdown(match[1] || target) || target;
        const displayName = path.posix.basename(stripAnchor(normalizedTarget)) || label;
        const nodeId = generateNodeId(this.filePath, 'import', `${normalizedTarget}:${lineNumber}:${column}`, lineNumber);
        const node: Node = {
          id: nodeId,
          kind: 'import',
          name: displayName,
          qualifiedName: `${this.filePath}::link:${normalizedTarget}:${lineNumber}:${column}`,
          filePath: this.filePath,
          language: 'markdown',
          signature: match[0],
          docstring: label,
          startLine: lineNumber,
          endLine: lineNumber,
          startColumn: column,
          endColumn: column + match[0].length,
          updatedAt: Date.now(),
        };

        this.nodes.push(node);
        this.edges.push({ source: owner.id, target: nodeId, kind: 'contains', provenance: 'heuristic' });
      } else {
        this.prunedTargets.push(normalizedTarget);
      }
      this.addReference(owner.id, normalizedTarget, 'imports', lineNumber, column);
    }
  }

  private extractPathMentions(line: string, lineNumber: number, owner: Node): void {
    const pathRegex = /(?<![\w/-])((?:\.{1,2}\/)?(?:[\w.-]+\/)*[\w.-]+\.(?:md|markdown|mdx|ts|tsx|js|mjs|cjs|py|sh|ps1|rb|go|rs|java|cs|php|swift|kt|kts|dart|vue|svelte|liquid|ya?ml|jsonc?|csv|toml))(?:#[\w.-]+)?/gi;
    let match: RegExpExecArray | null;

    while ((match = pathRegex.exec(line)) !== null) {
      const raw = match[0]!;
      if (line[match.index - 1] === '(') continue; // inline markdown link target, already handled
      if (line.slice(match.index + raw.length, match.index + raw.length + 2) === '::') continue;
      const normalizedTarget = this.normalizeReference(raw);
      if (!normalizedTarget) continue;

      if (this.shouldCreateLinkNode(normalizedTarget)) {
        const nodeId = generateNodeId(this.filePath, 'import', `${normalizedTarget}:${lineNumber}:${match.index}`, lineNumber);
        const node: Node = {
          id: nodeId,
          kind: 'import',
          name: path.posix.basename(stripAnchor(normalizedTarget)) || normalizedTarget,
          qualifiedName: `${this.filePath}::path:${normalizedTarget}:${lineNumber}:${match.index}`,
          filePath: this.filePath,
          language: 'markdown',
          signature: raw,
          startLine: lineNumber,
          endLine: lineNumber,
          startColumn: match.index,
          endColumn: match.index + raw.length,
          updatedAt: Date.now(),
        };

        this.nodes.push(node);
        this.edges.push({ source: owner.id, target: nodeId, kind: 'contains', provenance: 'heuristic' });
      } else {
        this.prunedTargets.push(normalizedTarget);
      }
      this.addReference(owner.id, normalizedTarget, 'references', lineNumber, match.index);
    }
  }

  private extractFileSymbolMentions(line: string, lineNumber: number, owner: Node, createImportNode = true): void {
    const fileSymbolRegex = /(?<![\w/-])((?:\.{1,2}\/)?(?:[\w.-]+\/)*[\w.-]+\.(?:md|markdown|mdx|ts|tsx|js|mjs|cjs|py|sh|ps1|rb|go|rs|java|cs|php|swift|kt|kts|dart|vue|svelte|liquid|ya?ml|jsonc?|csv|toml))::([A-Za-z_$][\w$]*(?:[./][A-Za-z_$][\w$]*)*)/g;
    let match: RegExpExecArray | null;

    while ((match = fileSymbolRegex.exec(line)) !== null) {
      if (line[match.index - 1] === '(') continue; // inline markdown link target, already handled
      const filePart = match[1]!;
      const symbols = splitSymbolList(match[2]!);

      for (const symbol of symbols) {
        const normalizedTarget = this.normalizeReference(`${filePart}::${symbol}`);
        if (!normalizedTarget) continue;
        const createNode = createImportNode && this.shouldCreateLinkNode(normalizedTarget);
        if (createImportNode && !createNode) this.prunedTargets.push(normalizedTarget);

        if (createNode) {
          const nodeId = generateNodeId(this.filePath, 'import', `${normalizedTarget}:${lineNumber}:${match.index}`, lineNumber);
          const node: Node = {
            id: nodeId,
            kind: 'import',
            name: `${path.posix.basename(filePart)}::${symbol}`,
            qualifiedName: `${this.filePath}::symbol-ref:${normalizedTarget}:${lineNumber}:${match.index}`,
            filePath: this.filePath,
            language: 'markdown',
            signature: match[0],
            startLine: lineNumber,
            endLine: lineNumber,
            startColumn: match.index,
            endColumn: match.index + match[0].length,
            updatedAt: Date.now(),
          };

          this.nodes.push(node);
          this.edges.push({ source: owner.id, target: nodeId, kind: 'contains', provenance: 'heuristic' });
        }

        this.addReference(owner.id, normalizedTarget, 'references', lineNumber, match.index);
      }
    }
  }

  private extractCommandsFromFence(fence: FenceState, fileNode: Node): void {
    if (!isShellLikeFence(fence.language)) return;
    // An unlabeled fence is as often output, a tree or a JSON snippet as a
    // command block. Only take a line that looks like a command from one;
    // a labeled shell fence keeps every line it has.
    const labeled = fence.language !== '';

    for (const entry of fence.lines) {
      const owner = this.findOwnerForLine(entry.line) ?? fileNode;
      // Every non-empty fence line keeps its search text, whether or not it is
      // a command: output and unusual commands were searchable as nodes before
      // the fold. Only recognized commands also join the summary and refs.
      const raw = entry.text.trim();
      if (raw) this.foldedText.push({ ownerId: owner.id, text: raw });

      const command = normalizeCommandLine(entry.text);
      if (!command) continue;
      if (!labeled && !looksLikeCommandLine(entry.text, command)) continue;

      const column = Math.max(0, entry.text.indexOf(command.split(/\s+/)[0]!));
      this.addCommand(command, entry.line, column, owner);
    }
  }

  private extractStructuredReferences(text: string, lineNumber: number, owner: Node): void {
    this.extractFileSymbolMentions(text, lineNumber, owner, false);
    this.extractPathReferencesOnly(text, lineNumber, owner);
    this.extractInlineCodeSymbolReferences(text, lineNumber, owner);
  }

  private extractPathReferencesOnly(text: string, lineNumber: number, owner: Node): void {
    const pathRegex = /(?<![\w/-])((?:\.{1,2}\/)?(?:[\w.-]+\/)*[\w.-]+\.(?:md|markdown|mdx|ts|tsx|js|mjs|cjs|py|sh|ps1|rb|go|rs|java|cs|php|swift|kt|kts|dart|vue|svelte|liquid|ya?ml|jsonc?|csv|toml))(?:#[\w.-]+)?/gi;
    let match: RegExpExecArray | null;

    while ((match = pathRegex.exec(text)) !== null) {
      const raw = match[0]!;
      if (text.slice(match.index + raw.length, match.index + raw.length + 2) === '::') continue;
      const normalizedTarget = this.normalizeReference(raw);
      if (!normalizedTarget) continue;
      this.addReference(owner.id, normalizedTarget, 'references', lineNumber, match.index);
    }
  }

  private extractInlineCodeSymbolReferences(text: string, lineNumber: number, owner: Node): void {
    const candidates = extractCodeSpans(text);
    for (const candidate of candidates) {
      const trimmed = candidate.trim();
      if (looksLikeCodeSymbol(trimmed)) {
        this.addReference(owner.id, trimmed, 'references', lineNumber, text.indexOf(candidate));
      }
    }
  }

  private findOwnerForLine(line: number): Node | null {
    let best: HeadingInfo | null = null;
    for (const heading of this.headings) {
      if (heading.line <= line && line <= heading.endLine) {
        if (!best || heading.level >= best.level) best = heading;
      }
    }
    return best?.node ?? null;
  }

  private addReference(
    fromNodeId: string,
    referenceName: string,
    referenceKind: EdgeKind,
    line: number,
    column: number
  ): void {
    const key = `${fromNodeId}:${referenceKind}:${referenceName}:${line}:${column}`;
    if (this.referenceKeys.has(key)) return;
    this.referenceKeys.add(key);
    this.unresolvedReferences.push({
      fromNodeId,
      referenceName,
      referenceKind,
      line,
      column,
      filePath: this.filePath,
      language: 'markdown',
    });
  }

  private normalizeReference(target: string): string | null {
    const [pathAndSymbol, anchorPart] = splitAnchor(target.trim());
    const [pathPart, symbolPart] = splitFileSymbol(pathAndSymbol);
    const cleanPath = decodePath(pathPart.split(/[?#]/)[0] ?? '');
    const anchor = anchorPart ? `#${slugifyHeading(anchorPart)}` : '';

    if (!cleanPath && anchor) {
      return `${this.filePath}${anchor}`;
    }
    if (!cleanPath) return null;

    const withoutLeadingSlash = cleanPath.startsWith('/') ? cleanPath.slice(1) : cleanPath;
    const baseDir = path.posix.dirname(this.filePath);
    const normalized = cleanPath.startsWith('/') || baseDir === '.'
      ? path.posix.normalize(withoutLeadingSlash)
      : path.posix.normalize(path.posix.join(baseDir, withoutLeadingSlash));

    if (normalized.startsWith('../') || normalized === '..') return null;
    return `${normalized}${symbolPart ? `::${symbolPart}` : ''}${anchor}`;
  }

  /**
   * Whether a target should get its own import node on a full index. The
   * resolver's file-path arm collects candidates by basename, so a target
   * whose basename no file node has cannot resolve to a file. Its reference
   * is still recorded (the resolver's route/module/symbol arms may match it,
   * and sync can retry it when a file appears), but the link node only added a
   * search child and a `contains` edge. Without a known-file set, or outside a
   * full index, every target keeps its node.
   */
  private shouldCreateLinkNode(normalizedTarget: string): boolean {
    if (!this.pruneMissingTargets || !this.knownFileNames) return true;
    const [pathAndSymbol] = splitAnchor(normalizedTarget);
    const [pathPart] = splitFileSymbol(pathAndSymbol);
    if (!pathPart || pathPart === this.filePath) return true;
    return this.knownFileNames.has(path.posix.basename(pathPart));
  }

  /**
   * Give each section (and the file node) its own `searchText`: the folded
   * rows, list items and fence lines its 600-character docstring preview did
   * not already include. Every owner keeps all of its text — there is no
   * file-wide truncation — and the separate column is indexed without being
   * displayed, so a search hit still lands on the section that owns the line.
   */
  private applySearchText(fileNode: Node): void {
    const owners = new Map<string, Node>([
      [fileNode.id, fileNode],
      ...this.headings.map((heading) => [heading.id, heading.node] as const),
    ]);
    const byOwner = new Map<string, string[]>();
    for (const { ownerId, text: raw } of this.foldedText) {
      const owner = owners.get(ownerId);
      if (!owner) continue;
      const text = stripInlineMarkdown(raw).replace(/\s+/g, ' ').trim();
      if (!text) continue;
      // Only text the owner's preview or command summary already carries is
      // skipped, on token boundaries so `CG-713` is not mistaken for a
      // substring of `XCG-713`.
      if (containsEntry(owner.docstring ?? '', text)) continue;
      const list = byOwner.get(ownerId) ?? [];
      list.push(text);
      byOwner.set(ownerId, list);
    }
    for (const [ownerId, texts] of byOwner) {
      const owner = owners.get(ownerId);
      if (!owner) continue;
      const seen = new Set<string>();
      const unique = texts.filter((text) => (seen.has(text) ? false : (seen.add(text), true)));
      if (unique.length > 0) owner.searchText = unique.join(' · ');
    }
  }

  /** Pruned link targets for the file digest's separate `links:` part. */
  private summarizePrunedTargets(): string[] {
    const seen = new Set<string>();
    const out: string[] = [];
    for (const target of this.prunedTargets) {
      const display = compactRefDisplay(target);
      if (!display || seen.has(display)) continue;
      seen.add(display);
      out.push(display);
      if (out.length >= 64) break;
    }
    return out;
  }

  private buildDocstring(startLine: number, endLine: number): string | undefined {
    const text = this.lines
      .slice(Math.max(0, startLine - 1), Math.max(0, endLine))
      .map((line) => stripInlineMarkdown(line.replace(/^#{1,6}\s+/, '').trim()))
      .filter((line) => line && !/^(```|~~~)/.test(line))
      .join('\n')
      .trim();

    if (!text) return undefined;
    return text.length > 600 ? `${text.slice(0, 600)}...` : text;
  }

  /**
   * Index of the closing `---` of a leading YAML frontmatter block, or -1 when
   * the file has none. Lines at or before this index are metadata, not body —
   * so they never produce headings and never seed the intro/digest.
   */
  private frontmatterEndIndex(): number {
    if ((this.lines[0] ?? '').trim() !== '---') return -1;
    for (let j = 1; j < this.lines.length; j++) {
      if (this.lines[j]!.trim() === '---') return j;
    }
    return -1;
  }

  /**
   * Replace the file node's docstring with a deterministic digest — a one-line
   * "what is this doc about" intro plus the key files/symbols it references.
   * It is derived purely from the already-extracted structure (no LLM), kept
   * short enough to surface in node details, and — because docstrings are in
   * the FTS index — makes a doc discoverable by the symbols it documents even
   * when the query matches no heading or filename. See the reviewer thread on
   * PR #361.
   */
  private finalizeFileDigest(fileNode: Node): void {
    const intro = this.buildIntro();
    const refs = this.collectKeyReferences();
    const links = this.summarizePrunedTargets();
    const commands = this.ownerCommands.get(fileNode.id) ?? [];
    const parts: string[] = [];
    if (intro) parts.push(intro);
    if (refs.length > 0) parts.push(`refs: ${refs.join(', ')}`);
    if (links.length > 0) {
      let text = links.join(', ');
      if (text.length > 800) text = `${text.slice(0, 797).trimEnd()}...`;
      parts.push(`links: ${text}`);
    }
    if (commands.length > 0) parts.push(`commands: ${summarizeCommands(commands)}`);
    const digest = parts.join('  ·  ').trim();
    fileNode.docstring = digest || this.buildDocstring(1, Math.min(this.lines.length, 40));
  }

  /**
   * First real prose line of the document — the closest deterministic stand-in
   * for "what this file is about". Skips frontmatter, headings, fenced code,
   * tables, badge/image-only lines, and raw HTML.
   */
  private buildIntro(): string | null {
    let fenceMarker: string | null = null;
    for (let i = this.frontmatterEndIndex() + 1; i < this.lines.length; i++) {
      const trimmed = this.lines[i]!.trim();
      const fence = /^(`{3,}|~{3,})/.exec(trimmed);
      if (fence) {
        const marker = fence[1]![0]!.repeat(fence[1]!.length);
        fenceMarker = fenceMarker === null ? marker : (trimmed.startsWith(fenceMarker) ? null : fenceMarker);
        continue;
      }
      if (fenceMarker !== null) continue;
      if (!trimmed) continue;
      if (/^#{1,6}\s/.test(trimmed)) continue;          // ATX heading
      if (/^(=+|-+|\*{3,}|_{3,})\s*$/.test(trimmed)) continue; // setext underline / hr
      if (trimmed.startsWith('|')) continue;             // table
      if (/^!\[/.test(trimmed)) continue;                // image / badge line
      if (/^<!--/.test(trimmed) || /^<[a-z]/i.test(trimmed)) continue; // html

      const text = stripInlineMarkdown(trimmed.replace(/^[-*+]\s+/, '').replace(/^>\s?/, ''));
      if (text) return truncateForSignature(text, 120);
    }
    return null;
  }

  /**
   * The documents/symbols this file points at, de-duplicated and compacted
   * (basename + `::symbol`/`#anchor`) in document order. Self-anchors and pure
   * in-page links are dropped. Includes targets pruned from the graph, so a
   * link to an unindexed asset still makes the file findable. Bounded so the
   * whole digest stays short enough to display.
   */
  private collectKeyReferences(maxItems = 12, maxChars = 150): string[] {
    const seen = new Set<string>();
    const out: string[] = [];
    let budget = maxChars;
    for (const ref of this.unresolvedReferences) {
      const referenceName = ref.referenceName;
      const [pathSym] = splitAnchor(referenceName);
      const [pathPart] = splitFileSymbol(pathSym);
      if (!pathPart || pathPart === this.filePath) continue; // pure/self anchor
      const display = compactRefDisplay(referenceName);
      if (!display || seen.has(display)) continue;
      if (out.length >= maxItems) break;
      if (out.length > 0 && budget - display.length - 2 < 0) break;
      seen.add(display);
      out.push(display);
      budget -= display.length + 2;
    }
    return out;
  }
}

function normalizeRelativePath(filePath: string): string {
  return filePath.replace(/\\/g, '/');
}

/**
 * Whether `line` can serve as the text of a Setext heading (the line sitting
 * directly above a `===`/`---` underline). Excludes anything that is itself a
 * block construct — blank lines, ATX headings, list items, blockquotes, table
 * rows, fences, underlines/rules, and HTML comments.
 */
function isSetextTextLine(line: string): boolean {
  const t = line.trim();
  if (!t) return false;
  if (/^#{1,6}\s/.test(t)) return false;          // ATX heading
  if (/^[-*+]\s/.test(t)) return false;           // unordered list item
  if (/^\d+[.)]\s/.test(t)) return false;         // ordered list item
  if (t.startsWith('>') || t.startsWith('|')) return false; // blockquote / table
  if (/^(`{3,}|~{3,})/.test(t)) return false;     // fence
  if (/^(=+|-+|\*{3,}|_{3,})\s*$/.test(t)) return false; // underline / thematic break
  if (/^<!--/.test(t)) return false;              // HTML comment
  return true;
}

/**
 * Compact a normalized reference target for the file digest: keep the basename
 * of the path plus any `::symbol` or `#anchor` suffix (e.g.
 * `phases/scripts/csv_search.py::run_p4` → `csv_search.py::run_p4`).
 */
function compactRefDisplay(referenceName: string): string | null {
  const [pathSym, anchor] = splitAnchor(referenceName);
  const [pathPart, symbol] = splitFileSymbol(pathSym);
  if (!pathPart) return null;
  const base = path.posix.basename(pathPart) || pathPart;
  if (symbol) return `${base}::${symbol}`;
  if (anchor) return `${base}#${anchor}`;
  return base;
}

function stripInlineMarkdown(value: string): string {
  const codeSpans: string[] = [];
  const withPlaceholders = value.replace(/`([^`]+)`/g, (_match, code: string) => {
    const token = `\uE000${codeSpans.length}\uE001`;
    codeSpans.push(code);
    return token;
  });

  return withPlaceholders
    .replace(/\[([^\]]+)\]\([^)]+\)/g, '$1')
    .replace(/[*~]/g, '')
    .replace(/\uE000(\d+)\uE001/g, (_match, index: string) => codeSpans[Number(index)] ?? '')
    .trim();
}

function slugifyHeading(value: string): string {
  const slug = stripInlineMarkdown(value)
    .toLowerCase()
    .replace(/<[^>]+>/g, '')
    .replace(/[^\p{L}\p{N}\s-]/gu, '')
    .trim()
    .replace(/\s+/g, '-');
  return slug || 'section';
}

function isLocalReference(target: string): boolean {
  const lower = target.toLowerCase();
  return !(
    lower.startsWith('http://') ||
    lower.startsWith('https://') ||
    lower.startsWith('mailto:') ||
    lower.startsWith('tel:') ||
    lower.startsWith('data:')
  );
}

function splitAnchor(target: string): [string, string] {
  const hash = target.indexOf('#');
  if (hash < 0) return [target, ''];
  return [target.slice(0, hash), target.slice(hash + 1)];
}

function stripAnchor(target: string): string {
  return splitAnchor(target)[0];
}

function splitFileSymbol(target: string): [string, string] {
  const marker = target.indexOf('::');
  if (marker < 0) return [target, ''];
  return [target.slice(0, marker), target.slice(marker + 2)];
}

function decodePath(value: string): string {
  try {
    return decodeURIComponent(value).replace(/\\/g, '/');
  } catch {
    return value.replace(/\\/g, '/');
  }
}

/**
 * Whether `haystack` contains `needle` on token boundaries. Markdown folding
 * uses this to decide a row/list item is already in its section preview; a
 * plain substring test would treat `CG-713` as present inside `XCG-713`.
 */
function containsEntry(haystack: string, needle: string): boolean {
  if (!haystack || !needle) return false;
  const escaped = needle.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  return new RegExp(`(^|[^\\w-])${escaped}([^\\w-]|$)`).test(haystack);
}

function isShellLikeFence(language: string): boolean {
  return [
    '',
    'sh',
    'shell',
    'bash',
    'zsh',
    'fish',
    'console',
    'terminal',
    'powershell',
    'ps1',
    'pwsh',
    'cmd',
    'bat',
  ].includes(language);
}

/** Commands kept per owning section/file (the searchable command summary). */
const MAX_COMMANDS_PER_OWNER = 8;
/** Cap on the `commands: ...` summary appended to a docstring. */
const MAX_COMMAND_SUMMARY_CHARS = 240;

/**
 * Command heads that make a line in an UNLABELED fence a command rather than
 * output, a tree or a JSON fragment. A labeled shell fence keeps every line.
 * The list is deliberately broad (coreutils plus the common toolchains); an
 * unusual head still stays in the section text, just not in its command
 * summary.
 */
const COMMAND_HEADS = new Set([
  'adb', 'apt', 'apt-get', 'aws', 'az', 'bandit', 'bash', 'biome', 'black', 'brew',
  'bun', 'bundle', 'cargo', 'cat', 'cc', 'chmod', 'chown', 'clang', 'cmake', 'cmd',
  'code', 'command', 'conda', 'corepack', 'coverage', 'cp', 'crontab', 'curl',
  'dart', 'deno', 'df', 'dnf', 'docker', 'docker-compose', 'dotnet', 'dropdb', 'du',
  'eas', 'env', 'eslint', 'eval', 'exec', 'expo', 'find', 'firebase', 'flake8',
  'fly', 'flyctl', 'fnm', 'free', 'g++', 'gcc', 'gcloud', 'gem', 'git', 'gh',
  'go', 'gofmt', 'goimports', 'gradle', 'grep', 'gunzip', 'gzip', 'head', 'helm',
  'htop', 'http', 'http-server', 'isort', 'java', 'javac', 'jest', 'jq', 'kubectl',
  'kind', 'kill', 'less', 'lerna', 'ln', 'make', 'mkdir', 'minikube', 'mongosh',
  'more', 'mount', 'mv', 'mvn', 'mysql', 'mypy', 'netlify', 'ninja', 'node', 'nohup',
  'npm', 'npx', 'nvm', 'nx', 'open', 'openssl', 'pip', 'pip3', 'pipenv', 'pkill',
  'pnpm', 'pod', 'podman', 'poetry', 'powershell', 'prettier', 'prisma', 'ps',
  'psql', 'pulumi', 'pwsh', 'py', 'pytest', 'python', 'python3', 'rails', 'rake',
  'rg', 'rm', 'rmdir', 'rsync',
  'ruby', 'ruff', 'rustc', 'scp', 'sed', 'serve', 'service', 'sh', 'snap', 'source',
  'sqlite3', 'ssh', 'ssh-keygen', 'sst', 'start', 'stripe', 'sudo', 'supabase',
  'swift', 'systemctl', 'tail', 'tar', 'terraform', 'time', 'top', 'touch', 'tox',
  'tsc', 'tsx', 'ts-node', 'turbo', 'type', 'umount', 'unzip', 'uv', 'vercel',
  'vite', 'vitest', 'watch', 'wget', 'whereis', 'which', 'wrangler', 'xargs',
  'xcrun', 'xcodebuild', 'yarn', 'yq', 'zsh', 'zip',
]);

/**
 * Whether a line in an unlabeled fence is a command. Prompts, relative
 * executables, `sudo`-style wrappers, shell assignments and known command
 * heads qualify; output lines, trees and JSON do not.
 */
function looksLikeCommandLine(raw: string, command: string): boolean {
  if (/^\s*(?:\$|>|PS>|PS\s+[A-Z]:\\[^>]*>)\s/i.test(raw)) return true;
  const first = command.split(/\s+/)[0] ?? '';
  if (first.startsWith('./') || first.startsWith('/')) return true;
  if (/^[A-Za-z_][A-Za-z0-9_]*=/.test(first)) return true;
  return COMMAND_HEADS.has(first.toLowerCase());
}

function summarizeCommands(commands: string[]): string {
  const text = commands.join(', ');
  return text.length > MAX_COMMAND_SUMMARY_CHARS
    ? `${text.slice(0, MAX_COMMAND_SUMMARY_CHARS - 3).trimEnd()}...`
    : text;
}

function isTableHeaderLine(line: string): boolean {
  return isTableRowLine(line) && parseTableCells(line).some(Boolean);
}

function isTableSeparatorLine(line: string): boolean {
  const cells = parseTableCells(line);
  return cells.length >= 2 && cells.every((cell) => /^:?-{3,}:?$/.test(cell.trim()));
}

function isTableRowLine(line: string): boolean {
  return line.trim().startsWith('|') && line.includes('|');
}

function parseTableCells(line: string): string[] {
  const trimmed = line.trim();
  const withoutOuter = trimmed.startsWith('|') && trimmed.endsWith('|')
    ? trimmed.slice(1, -1)
    : trimmed;
  const cells: string[] = [];
  let current = '';
  let inCode = false;

  for (let i = 0; i < withoutOuter.length; i++) {
    const char = withoutOuter[i]!;
    if (char === '`') {
      inCode = !inCode;
      current += char;
      continue;
    }
    if (char === '|' && !inCode) {
      cells.push(current.trim());
      current = '';
      continue;
    }
    current += char;
  }

  cells.push(current.trim());
  return cells;
}

function shouldIndexTableRow(headers: string[], cells: string[]): boolean {
  const text = `${headers.join(' ')} ${cells.join(' ')}`;
  return Boolean(
    stableIdentifier(text) ||
    /(?:^|[\s`])(?:python|python3|py|node|tsx|ts-node|deno|bun|ruby|bash|sh|pwsh|powershell)\s+/i.test(text) ||
    /[\w.-]+\.(?:md|markdown|mdx|ts|tsx|js|mjs|cjs|py|sh|ps1|rb|go|rs|java|cs|php|swift|kt|kts|dart|vue|svelte|liquid|ya?ml|jsonc?|csv|toml)(?:[#:]|[\s`]|$)/i.test(text)
  );
}

function shouldIndexListItem(text: string): boolean {
  return Boolean(
    stableIdentifier(text) ||
    /[\w.-]+\.(?:md|markdown|mdx|ts|tsx|js|mjs|cjs|py|sh|ps1|rb|go|rs|java|cs|php|swift|kt|kts|dart|vue|svelte|liquid|ya?ml|jsonc?|csv|toml)::[A-Za-z_$][\w$]*/.test(text)
  );
}

function stableIdentifier(text: string): string | null {
  const matches = text.match(/\b[A-Z][A-Z0-9]{1,}(?:-[A-Z0-9]+)+\b|\b[A-Z]{2,}[0-9]+(?:-[A-Z0-9]+)*\b/g);
  return matches?.[0] ?? null;
}

function truncateForSignature(text: string, max: number): string {
  const stripped = stripInlineMarkdown(text).replace(/\s+/g, ' ').trim();
  if (stripped.length <= max) return stripped;
  return `${stripped.slice(0, max - 3).trimEnd()}...`;
}

function extractCodeSpans(text: string): string[] {
  const spans: string[] = [];
  const regex = /`([^`]+)`/g;
  let match: RegExpExecArray | null;
  while ((match = regex.exec(text)) !== null) {
    if (match[1]) spans.push(match[1]);
  }
  return spans;
}

function extractCommandFromText(text: string): string | null {
  const command = normalizeCommandLine(stripInlineMarkdown(text));
  if (!command) return null;
  return extractScriptPath(command) ||
    /^(?:python|python3|py|node|tsx|ts-node|deno|bun|ruby|bash|sh|pwsh|powershell)\s+["']?\{script_path\}["']?/i.test(command) ||
    /^(?:npm|pnpm|yarn)\s+(?:run\s+)?[\w:.-]+/.test(command)
    ? command
    : null;
}

function splitSymbolList(value: string): string[] {
  return value
    .split(/[,\s]+/)
    .map((part) => part.trim().replace(/^::/, ''))
    .filter((part) => /^[A-Za-z_$][\w$]*(?:[./][A-Za-z_$][\w$]*)*$/.test(part));
}

function looksLikeCodeSymbol(value: string): boolean {
  if (!value || value.length > 140 || /\s/.test(value)) return false;
  if (/^(?:true|false|null|undefined|none|yes|no)$/i.test(value)) return false;
  if (/^[A-Z0-9_-]+$/.test(value) && value.includes('-')) return false;
  return /^[A-Za-z_$][\w$]*(?:(?:\.|::)[A-Za-z_$][\w$]*)+$/.test(value);
}

function normalizeCommandLine(line: string): string | null {
  const trimmed = line.trim();
  if (!trimmed || trimmed.startsWith('#') || trimmed.startsWith('//')) return null;

  const withoutPrompt = trimmed
    .replace(/^(?:\$|>)\s+/, '')
    .replace(/^PS\s+[A-Z]:\\[^>]*>\s*/i, '')
    .replace(/^PS>\s*/i, '');

  if (!withoutPrompt || /^(cd|echo|export|set)\b/.test(withoutPrompt)) return null;
  return withoutPrompt;
}

function extractScriptPath(command: string): string | null {
  const patterns = [
    /^(?:node|tsx|ts-node|bun)\s+(?:run\s+)?(?:--[\w=-]+\s+)*(['"]?)([^'"\s]+\.(?:js|mjs|cjs|ts|tsx))\1/,
    /^(?:deno)\s+run\s+(?:--[\w=-]+\s+)*(['"]?)([^'"\s]+\.(?:js|mjs|ts|tsx))\1/,
    /^(?:python|python3|py|ruby|bash|sh|pwsh|powershell)\s+(?:-[\w-]+\s+)*(['"]?)([^'"\s]+\.(?:py|rb|sh|ps1))\1/,
  ];

  for (const pattern of patterns) {
    const match = pattern.exec(command);
    if (match?.[2]) return match[2];
  }

  return null;
}
