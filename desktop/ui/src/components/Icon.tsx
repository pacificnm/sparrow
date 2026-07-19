import { FontAwesomeIcon } from "@fortawesome/react-fontawesome";
import type { IconDefinition } from "@fortawesome/fontawesome-svg-core";

type IconProps = {
  icon: IconDefinition;
  className?: string;
  title?: string;
};

/** Font Awesome icon wrapper (size via Tailwind `text-*` / `size-*` on className). */
export function Icon({ icon, className, title }: IconProps) {
  return (
    <FontAwesomeIcon
      icon={icon}
      className={className}
      title={title}
      aria-hidden={title ? undefined : true}
    />
  );
}
