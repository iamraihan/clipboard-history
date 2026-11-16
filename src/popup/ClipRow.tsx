import { FileText, Files, Image as ImageIcon, Pin, Trash2, Type } from "lucide-react";
import type { ClipKind, ClipSummary } from "../lib/types";

const KIND_ICON: Record<ClipKind, typeof Type> = {
  text: Type,
  richText: FileText,
  image: ImageIcon,
  files: Files,
};

interface Props {
  clip: ClipSummary;
  selected: boolean;
  onPick: () => void;
  onTogglePin: () => void;
  onDelete: () => void;
  onHover: () => void;
}

export function ClipRow({ clip, selected, onPick, onTogglePin, onDelete, onHover }: Props) {
  const Icon = KIND_ICON[clip.kind];
  const label = clip.preview || (clip.kind === "image" ? "Image" : "Empty item");
  return (
    <div
      role="option"
      aria-selected={selected}
      id={`clip-${clip.id}`}
      data-selected={selected}
      onClick={onPick}
      onMouseMove={onHover}
      className="group relative mx-1 flex h-9 cursor-default items-center gap-2 rounded-md px-2 data-[selected=true]:bg-selected data-[selected=true]:outline-2 data-[selected=true]:-outline-offset-2 data-[selected=true]:outline-accent"
    >
      {clip.thumbnailPngBase64 ? (
        <img
          alt=""
          src={`data:image/png;base64,${clip.thumbnailPngBase64}`}
          className="h-7 w-7 shrink-0 rounded-sm object-cover"
        />
      ) : (
        <Icon aria-hidden className="h-4 w-4 shrink-0 text-muted" />
      )}
      <span className="min-w-0 flex-1 truncate">{label}</span>
      {clip.pinned && <Pin aria-label="Pinned" className="h-3.5 w-3.5 shrink-0 fill-accent text-accent" />}
      <span className="hidden shrink-0 items-center gap-0.5 group-data-[selected=true]:flex group-hover:flex">
        <RowButton label={clip.pinned ? "Unpin" : "Pin"} onClick={onTogglePin}>
          <Pin className="h-3.5 w-3.5" />
        </RowButton>
        <RowButton label="Delete" onClick={onDelete}>
          <Trash2 className="h-3.5 w-3.5" />
        </RowButton>
      </span>
    </div>
  );
}

function RowButton(props: { label: string; onClick: () => void; children: React.ReactNode }) {
  return (
    <button
      type="button"
      tabIndex={-1}
      aria-label={props.label}
      title={props.label}
      onClick={(e) => {
        e.stopPropagation();
        props.onClick();
      }}
      className="grid h-6 w-6 place-items-center rounded text-muted hover:bg-hover hover:text-fg"
    >
      {props.children}
    </button>
  );
}
