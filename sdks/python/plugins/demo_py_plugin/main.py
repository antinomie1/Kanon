"""Demonstration Python plugin for Kanon microkernel."""

import asyncio
import random
from typing import Any, Dict, Optional

from kanon_sdk import (
    CommandEvent,
    MessageEvent,
    MessageSegment,
    Plugin,
    PluginContext,
    command,
    tool,
    trigger,
)
from kanon_sdk.proto import pb


class DemoPythonPlugin(Plugin):
    """Demonstration plugin implemented in Python."""

    id = "org.kanon.plugin.demo_py"
    name = "Demo Python Plugin"
    version = "0.1.0"
    author = "Kanon Dev"
    description = "Demonstration plugin written in Python"
    priority = 100

    async def on_load(self, ctx: PluginContext) -> None:
        print(f"Demo Python Plugin initialized with data directory: {ctx.data_dir}", flush=True)

    async def on_pre_filter(
        self,
        req: pb.PipelineEventRequest,
    ) -> Optional[pb.PreFilterResult]:
        if "[block]" in req.raw_text:
            return pb.PreFilterResult(
                action=pb.PreFilterResult.Action.BLOCK,
                modified_text="",
                reply_messages=[
                    MessageSegment.text("Message blocked by Demo Python Plugin pre-filter")
                ],
            )
        return None

    @command(
        name="pycalc",
        description="Python-based calculation command",
        usage="/pycalc <expr>",
        priority=100,
        aliases=("pc",),
    )
    async def handle_pycalc(self, event: CommandEvent) -> str:
        # Returning text is the shortest way to answer.
        return f"Python calculation result for [{' '.join(event.args)}]: 42 (fast-path)"

    @command(name="guess", description="Guess a number between 1 and 10", usage="/guess")
    async def handle_guess(self, event: CommandEvent) -> None:
        # A multi-turn conversation: each wait_next sends the replies so far and resumes with the
        # same sender's next message in this channel.
        secret = random.randint(1, 10)
        await event.reply("I picked a number between 1 and 10. Your guess?")
        for _ in range(3):
            try:
                answer = await event.wait_next(timeout=60)
            except asyncio.TimeoutError:
                await event.reply(f"Time is up — it was {secret}.")
                return
            if not answer.text.strip().isdigit():
                await answer.reply("Please answer with a number.")
                continue
            guess = int(answer.text.strip())
            if guess == secret:
                await answer.reply("Correct!")
                return
            await answer.reply("Higher." if guess < secret else "Lower.")
        await event.reply(f"Out of guesses — it was {secret}.")

    @trigger(r"^(?:hi|hello) py$", description="Greets back")
    async def greet(self, event: MessageEvent) -> list:
        return [MessageSegment.quote(event.event_id), MessageSegment.text("Hello from Python!")]

    @tool(
        name="py_calc",
        description="Python mathematical calculation tool",
        parameters={
            "type": "object",
            "properties": {
                "expr": {"type": "string", "description": "Expression to evaluate"},
            },
        },
    )
    async def handle_py_calc(self, params: Dict[str, Any]) -> Dict[str, Any]:
        return {
            "result": 42.0,
            "summary": "Calculated via Python plugin tool",
        }
