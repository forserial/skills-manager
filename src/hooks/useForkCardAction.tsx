import { useState } from "react";
import { GitFork } from "lucide-react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { useApp } from "../context/AppContext";
import type { CardAction } from "../components/CardActionMenu";
import { ForkSkillDialog } from "../components/ForkSkillDialog";
import type { ManagedSkill } from "../lib/tauri";
import { getErrorMessage } from "../lib/error";
import { forkSkill } from "../lib/watchedUpstreamsApi";

/** The "Fork" entry of a skill card's action menu (sm-69q.7), plus the name
 *  dialog it opens. Render `dialog` once; call `forkAction` per card. A
 *  successful fork refreshes the library and opens the new skill. */
export function useForkCardAction(): {
  forkAction: (skill: ManagedSkill) => CardAction;
  dialog: React.ReactNode;
} {
  const { t } = useTranslation();
  const { managedSkills, refreshManagedSkills, openSkillDetailById } = useApp();
  const [target, setTarget] = useState<ManagedSkill | null>(null);

  const handleFork = async (newName: string) => {
    if (!target) return;
    try {
      const result = await forkSkill(target.id, newName);
      await refreshManagedSkills();
      toast.success(t("fork.done", { name: result.name }), {
        description: result.watched ? t("fork.doneWatching") : t("fork.doneNoSource"),
      });
      openSkillDetailById(result.skill_id);
    } catch (error) {
      toast.error(getErrorMessage(error, t("common.error")));
      throw error;
    }
  };

  return {
    forkAction: (skill) => ({
      key: "fork",
      label: t("fork.menu"),
      icon: <GitFork className="h-3.5 w-3.5" />,
      onSelect: () => setTarget(skill),
    }),
    dialog: (
      <ForkSkillDialog
        skillName={target?.name ?? null}
        existingNames={managedSkills.map((s) => s.name)}
        onClose={() => setTarget(null)}
        onFork={handleFork}
      />
    ),
  };
}
