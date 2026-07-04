<?php

declare(strict_types=1);

namespace ForgeLib;

final class ForgeLib
{
    public const STATUS = 'metadata-only';
    public const REPOSITORY = 'https://github.com/isala404/forge';

    public static function notice(): string
    {
        return 'Forge PHP bindings are planned. Use Rust, Node.js, or Python today.';
    }
}
