import { useState } from "react";
import { Pin } from "lucide-react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import type { ManagedSkill } from "../lib/tauri";
import { getErrorMessage } from "../lib/error";
import { useApp } from "../context/AppContext";
import { isPinnable, pinSkill, unpinSkill, type PinState } from "../lib/pinningApi";
import { ToggleSwitch } from "./ToggleSwitch";

const pillClass =
  "inline-flex shrink-0 items-center gap-1.5 rounded-full border border-border-subtle bg-bg-secondary px-2 py-1 text-[12px] text-muted";

/** "Pinned": hold a git skill at the revision it is at (sm-69q.5). A pinned
 *  skill is still checked, but no update path moves it until it is unpinned.
 *  Hidden for skills with no upstream revision. */
export function SkillPinSection({ skill }: { skill: ManagedSkill }) {
  const { t } = useTranslation();
  const { refreshManagedSkills } = useApp();
  const [pin, setPin] = useState<PinState | null>(skill.pin?.pinned ? skill.pin : null);
  const [saving, setSaving] = useState(false);

  if (!isPinnable(skill.source_type)) return null;

  const pinned = !!pin;
  const revision = (pin?.revision ?? skill.source_revision)?.slice(0, 7) ?? null;
  const pendingUpdate = skill.update_status === "update_available";

  const handleToggle = async () => {
    setSaving(true);
    try {
      if (pinned) {
        await unpinSkill(skill.id);
        setPin(null);
        toast.success(t("pin.unpinnedToast", { name: skill.name }));
      } else {
        setPin(await pinSkill(skill.id));
        toast.success(t("pin.pinnedToast", { name: skill.name }));
      }
      await refreshManagedSkills();
    } catch (error) {
      toast.error(getErrorMessage(error, t("common.error")));
    } finally {
      setSaving(false);
    }
  };

  return (
    <section className="mb-4 rounded-xl border border-border-subtle bg-surface/70">
      <div className="flex items-center justify-between gap-3 px-4 py-3">
        <span className="flex min-w-0 items-center gap-2">
          <span className={pillClass}>
            <Pin className="h-3.5 w-3.5" />
            {revision ?? t("pin.unknownRevision")}
          </span>
          <span className="truncate text-[13px] font-medium text-secondary">
            {pinned ? t("pin.pinnedAt", { revision: revision ?? "?" }) : t("pin.title")}
          </span>
          {pinned && pendingUpdate && (
            <span className="inline-flex shrink-0 items-center rounded-full bg-amber-500/10 px-2 py-0.5 text-[11px] font-medium text-amber-600 dark:text-amber-400">
              {t("pin.updateAvailable")}
            </span>
          )}
        </span>
        <ToggleSwitch
          checked={pinned}
          loading={saving}
          onChange={handleToggle}
          title={pinned ? t("pin.unpin") : t("pin.pin")}
        />
      </div>
      <p className="border-t border-border-subtle px-4 py-2.5 text-[12px] text-muted">
        {pinned
          ? pin?.note
            ? t("pin.pinnedHintWithNote", { note: pin.note })
            : t("pin.pinnedHint")
          : t("pin.unpinnedHint")}
      </p>
    </section>
  );
}
