/**
 * Simulated project folders for the web demo: a Windows-style workstation
 * (drive letters, `\`) and a POSIX build server. Paths are built here, the
 * way a real server builds them; the UI only sends them back.
 */
import type {
  ErrorCode,
  FolderCrumb,
  FolderEntry,
  FolderListing,
  HostOs,
  ProjectFolders,
} from './types';
import type { MockProfile } from './mockData';

interface Node {
  name: string;
  git?: boolean;
  history?: boolean;
  children?: Node[];
}

interface MockTree {
  os: HostOs;
  roots: { path: string; node: Node }[];
  /** Recent project paths, most recent first. */
  recent: string[];
}

const MAX_ENTRIES = 500;

const dir = (name: string, children: Node[] = [], extra: Partial<Node> = {}): Node => ({
  name,
  children,
  ...extra,
});
const repo = (name: string, children: Node[] = [], history = true): Node =>
  dir(name, [dir('.git'), dir('src'), dir('node_modules'), ...children], { git: true, history });

/** A folder with many sub-folders, to show truncation. */
const big = (name: string, count: number): Node =>
  dir(
    name,
    Array.from({ length: count }, (_, i) => dir(`photo-${String(i + 1).padStart(4, '0')}`)),
  );

function trees(): Record<MockProfile, MockTree> {
  return {
    workstation: {
      os: 'windows',
      roots: [
        {
          path: 'C:\\Users\\dev',
          node: dir('dev', [
            dir('.vscode'),
            dir('AppData'),
            dir('code', [
              repo('payments-api', [dir('migrations')]),
              repo('web-dashboard'),
              repo('docs-site'),
              repo('infra', [dir('terraform')]),
              repo('mobile-app', [], false),
              dir('playground', [dir('scratch')]),
            ]),
            dir('Desktop'),
            dir('Documents', [dir('notes')]),
            dir('Downloads'),
            big('Pictures', 620),
          ]),
        },
        {
          path: 'D:\\',
          node: dir('D:\\', [
            dir('$RECYCLE.BIN'),
            dir('System Volume Information'),
            dir('work', [repo('client-portal'), repo('data-pipeline', [], false)]),
          ]),
        },
        {
          path: 'E:\\',
          node: dir('E:\\', [dir('Voice-Command-AI', [], { git: true, history: true })]),
        },
      ],
      recent: [
        'C:\\Users\\dev\\code\\payments-api',
        'C:\\Users\\dev\\code\\web-dashboard',
        'E:\\Voice-Command-AI',
        'C:\\Users\\dev\\code\\docs-site',
        'D:\\work\\client-portal',
        'C:\\Users\\dev\\code\\infra',
      ],
    },
    build_server: {
      os: 'linux',
      roots: [
        {
          path: '/srv/work',
          node: dir('work', [
            repo('monorepo', [dir('services'), dir('target')]),
            repo('legacy-billing'),
            repo('perf-lab'),
            dir('tmp'),
          ]),
        },
        {
          path: '/home/build',
          node: dir('build', [dir('.cache'), dir('scripts'), repo('ci-config', [], false)]),
        },
      ],
      recent: ['/srv/work/monorepo', '/srv/work/legacy-billing', '/srv/work/perf-lab'],
    },
  };
}

export class MockFolderError extends Error {
  constructor(
    readonly code: ErrorCode,
    message: string,
  ) {
    super(message);
  }
}

const HIDDEN = new Set(['node_modules', 'target']);
const SYSTEM = new Set(['$recycle.bin', 'system volume information']);

export class MockFolders {
  private readonly tree: MockTree;
  private readonly sep: string;

  constructor(profile: MockProfile) {
    this.tree = trees()[profile];
    this.sep = this.tree.os === 'windows' ? '\\' : '/';
  }

  private join(parent: string, name: string): string {
    return parent.endsWith(this.sep) ? parent + name : parent + this.sep + name;
  }

  private key(path: string): string {
    const p = this.tree.os === 'windows' ? path.replace(/\//g, '\\').toLowerCase() : path;
    return p.length > 3 && p.endsWith(this.sep) ? p.slice(0, -1) : p;
  }

  /** The root and node at `path`, with the names walked below the root. */
  private find(path: string): { root: string; node: Node; names: string[] } {
    const key = this.key(path);
    if (key.split(this.sep).includes('..')) {
      throw new MockFolderError('forbidden', `${path} is outside the allowed workspace roots`);
    }
    for (const r of this.tree.roots) {
      const rootKey = this.key(r.path);
      const prefix = rootKey.endsWith(this.sep) ? rootKey : rootKey + this.sep;
      if (key !== rootKey && !key.startsWith(prefix)) continue;
      const rest = key === rootKey ? [] : key.slice(prefix.length).split(this.sep).filter(Boolean);
      let node = r.node;
      const names: string[] = [];
      for (const part of rest) {
        const next = node.children?.find((c) =>
          this.tree.os === 'windows' ? c.name.toLowerCase() === part : c.name === part,
        );
        if (!next) throw new MockFolderError('not_found', `folder does not exist: ${path}`);
        node = next;
        names.push(next.name);
      }
      return { root: r.path, node, names };
    }
    throw new MockFolderError('forbidden', `${path} is outside the allowed workspace roots`);
  }

  private entry(path: string, node: Node, lastUsed: number | null = null): FolderEntry {
    return {
      name: node.name,
      path,
      is_git_repo: !!node.git,
      has_agent_history: !!node.history,
      last_used: lastUsed,
    };
  }

  projectFolders(now: number): ProjectFolders {
    return {
      roots: this.tree.roots.map((r) => ({ ...this.entry(r.path, r.node), name: r.path })),
      recent: this.tree.recent.map((p, i) => {
        const { node } = this.find(p);
        return this.entry(p, node, now - (i + 1) * 3_600_000);
      }),
      os: this.tree.os,
      separator: this.sep,
    };
  }

  listFolder(path: string, showHidden = false): FolderListing {
    const { root, node, names } = this.find(path);
    let current = root;
    const breadcrumbs: FolderCrumb[] = [{ name: root, path: root }];
    for (const name of names) {
      current = this.join(current, name);
      breadcrumbs.push({ name, path: current });
    }
    const parent = breadcrumbs.length > 1 ? breadcrumbs[breadcrumbs.length - 2]!.path : null;
    const visible = (node.children ?? [])
      .filter((c) => !SYSTEM.has(c.name.toLowerCase()))
      .filter((c) => showHidden || !(c.name.startsWith('.') || HIDDEN.has(c.name)))
      .sort((a, b) => a.name.toLowerCase().localeCompare(b.name.toLowerCase()));
    return {
      path: current,
      // Like `Path::file_name`: the last folder, or the whole drive root.
      name: names.length
        ? names[names.length - 1]!
        : /^[A-Za-z]:\\?$|^\/$/.test(root)
          ? root
          : root.split(this.sep).filter(Boolean).pop()!,
      parent,
      root,
      breadcrumbs,
      entries: visible.slice(0, MAX_ENTRIES).map((c) => this.entry(this.join(current, c.name), c)),
      truncated: visible.length > MAX_ENTRIES,
    };
  }

  folderPath(parent: string, rawName: string): { path: string; exists: boolean } {
    const name = rawName.trim();
    const bad =
      !name ||
      name === '.' ||
      name === '..' ||
      /[/\\\p{Cc}]/u.test(name) ||
      (this.tree.os === 'windows' && /[<>:"|?*]|\.$/.test(name));
    if (bad) throw new MockFolderError('invalid_request', `not a valid folder name: ${rawName}`);
    const at = this.listFolder(parent, true);
    const exists = at.entries.some((e) =>
      this.tree.os === 'windows' ? e.name.toLowerCase() === name.toLowerCase() : e.name === name,
    );
    return { path: this.join(at.path, name), exists };
  }
}
