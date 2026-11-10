import { useTranslation } from "react-i18next";
import { ConfirmDialog } from "./ConfirmDialog";
import type { OverwriteReason, PendingOverwrite, PendingRemoval } from "../lib/tauri";

/**
 * The confirmation for a held update or re-import: what it would remove and,
 * since sm-69q.4, which local edits it would replace. Confirming calls the
 * update again with the approval token, which approves exactly these lists.
 *
 * Every path is shown, never a sample: recognising one's own file is the
 * whole point, and it might be the twenty-first.
 */
interface Props {
  open: boolean;
  skillName: string;
  removals: PendingRemoval[];
  overwrites?: PendingOverwrite[];
  reason?: OverwriteReason | null;
  onClose: () => void;
  onConfirm: () => Promise<void>;
}

function Group({ label, paths }: { label: string; paths: string[] }) {
  return (
    <div className="mb-4 min-h-0" role="group" aria-label={label}>
      <p className="text-[12px] font-medium text-secondary mb-2">{label}</p>
      <div className="flex flex-wrap gap-2 overflow-y-auto max-h-40">
        {paths.map((path) => (
          <span
            key={path}
            className="rounded-full border border-border-subtle bg-bg-secondary px-2.5 py-1 text-[13px] text-secondary"
          >
            {path}
          </span>
        ))}
      </div>
    </div>
  );
}

export function HeldUpdateDialog({
  open,
  skillName,
  removals,
  overwrites = [],
  reason,
  onClose,
  onConfirm,
}: Props) {
  const { t } = useTranslation();
  const removalPaths = removals.map((r) =>
    r.location === "library" ? r.path : `${r.location}: ${r.path}`
  );

  // Removals only: exactly the dialog as it was before sm-69q.4.
  if (overwrites.length === 0) {
    return (
      <ConfirmDialog
        open={open}
        tone="warning"
        title={t("mySkills.updateActions.removalTitle")}
        message={t("mySkills.updateActions.removalMessage", {
          name: skillName,
          count: removals.length,
        })}
        details={removalPaths}
        confirmLabel={t("mySkills.updateActions.removalConfirm")}
        onClose={onClose}
        onConfirm={onConfirm}
      />
    );
  }

  return (
    <ConfirmDialog
      open={open}
      tone="warning"
      title={t("heldUpdate.title")}
      message={t("heldUpdate.message", { name: skillName, count: overwrites.length })}
      confirmLabel={t("heldUpdate.confirm")}
      onClose={onClose}
      onConfirm={onConfirm}
    >
      {reason && reason !== "local_changes" ? (
        <p className="text-[12px] text-tertiary mb-4">{t(`heldUpdate.reason.${reason}`)}</p>
      ) : null}
      <Group
        label={t("heldUpdate.overwritesGroup", { count: overwrites.length })}
        paths={overwrites.map((o) => o.path)}
      />
      {removalPaths.length > 0 ? (
        <Group
          label={t("heldUpdate.removalsGroup", { count: removalPaths.length })}
          paths={removalPaths}
        />
      ) : null}
    </ConfirmDialog>
  );
}
