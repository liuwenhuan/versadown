import { formatBytes } from '../lib/downloadEvents';
import { ConfirmResource } from '../lib/types';

const videoExtension = /\.(?:3gp|avi|flv|m4v|mkv|mov|mp4|mpeg|mpg|rmvb|ts|webm|wmv)$/i;

export function resourceFileCategory(resource: ConfirmResource): 'video' | 'other' {
  return videoExtension.test(resource.name) ? 'video' : 'other';
}

export function displayResourceSize(value: string): string {
  const matches = [...value.matchAll(/(\d+)\s*B\b/gi)];
  const bytes = Number(matches.at(-1)?.[1]);
  return Number.isSafeInteger(bytes) ? formatBytes(bytes) : value;
}

function resourceExtension(name: string): string {
  const leaf = name.replaceAll('\\', '/').split('/').at(-1) ?? name;
  const dot = leaf.lastIndexOf('.');
  return dot > 0 && dot < leaf.length - 1 ? leaf.slice(dot + 1).toLowerCase() : '—';
}

function ResourceFileIcon({ resource }: { resource: ConfirmResource }) {
  const category = resourceFileCategory(resource);
  const extension = resource.name.split('.').pop()?.slice(0, 4).toUpperCase();
  return <span className={`parse-file-icon ${category}`} aria-hidden="true">{category === 'video' ? '▶' : extension || '文'}</span>;
}

export function ResourceFileRow({ resource, displayName = resource.name, disabled = false, showExtension = false, compact = false, onToggle }: {
  resource: ConfirmResource;
  displayName?: string;
  disabled?: boolean;
  showExtension?: boolean;
  compact?: boolean;
  onToggle(): void;
}) {
  return <label className={`resource${resource.selected ? ' selected' : ''}${compact ? ' batch-resource-row' : ''}`}>
    <input type="checkbox" aria-label={`选择 ${displayName}`} checked={resource.selected} disabled={disabled} onChange={onToggle} />
    <ResourceFileIcon resource={resource} />
    <span title={resource.name}>{displayName}</span>
    {showExtension && <small className="resource-extension">{resourceExtension(displayName)}</small>}
    <small className="resource-size">{displayResourceSize(resource.size)}</small>
  </label>;
}
