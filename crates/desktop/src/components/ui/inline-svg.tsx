type InlineSvgProps = {
	/** Raw markup of a BUNDLED static SVG (a `?raw` import). Never pass
	 * runtime or network content: it is injected unescaped. */
	svg: string;
	as?: "div" | "span";
} & Omit<
	React.HTMLAttributes<HTMLElement>,
	"children" | "dangerouslySetInnerHTML"
>;

function InlineSvg({ svg, as: Tag = "span", ...props }: InlineSvgProps) {
	return (
		<Tag
			{...props}
			// eslint-disable-next-line @eslint-react/dom-no-dangerously-set-innerhtml
			dangerouslySetInnerHTML={{ __html: svg }}
		/>
	);
}

export { InlineSvg };
