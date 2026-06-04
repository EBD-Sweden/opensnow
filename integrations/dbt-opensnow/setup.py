from setuptools import find_namespace_packages, setup

package_name = "dbt-opensnow"
package_version = "0.1.0"
description = "The OpenSnow adapter plugin for dbt"

setup(
    name=package_name,
    version=package_version,
    description=description,
    long_description=description,
    author="OpenSnow",
    author_email="hello@opensnow.dev",
    url="https://github.com/opensnow/opensnow",
    packages=find_namespace_packages(include=["dbt", "dbt.*"]),
    include_package_data=True,
    install_requires=[
        "dbt-core>=1.7,<2.0",
        "dbt-postgres>=1.7,<2.0",
        "psycopg2-binary>=2.9,<3.0",
    ],
    zip_safe=False,
    classifiers=[
        "Development Status :: 3 - Alpha",
        "License :: OSI Approved :: Apache Software License",
        "Operating System :: OS Independent",
        "Programming Language :: Python :: 3",
        "Programming Language :: Python :: 3.9",
        "Programming Language :: Python :: 3.10",
        "Programming Language :: Python :: 3.11",
        "Programming Language :: Python :: 3.12",
    ],
    python_requires=">=3.9",
    entry_points={
        "dbt.adapter": [
            "opensnow = dbt.adapters.opensnow",
        ],
    },
)
